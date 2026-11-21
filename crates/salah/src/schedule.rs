// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//

//! # Prayer Schedule
//!
//! This module provides the main objects that are used for calculating
//! the prayer times.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;
use chrono::{DateTime, Datelike, NaiveDate, TimeDelta, TimeZone, Utc};

use crate::astronomy::ops;
use crate::astronomy::solar::SolarTime;
use crate::astronomy::unit::Angle;
use crate::error::Error;
use crate::models::coordinates::Coordinates;
use crate::models::method::Method;
use crate::models::parameters::{DepressionAngle, Isha, Parameters};
use crate::models::polar_circle_resolution::PolarCircleResolution;
use crate::models::prayer::Prayer;
use crate::models::rounding::Rounding;
use crate::time::Moment;

/// The first date prayer times are calculated for.
pub const FIRST_DATE: NaiveDate = NaiveDate::from_ymd_opt(1000, 1, 1).unwrap();
/// The last date prayer times are calculated for. The astronomical series
/// are Meeus's, made for the centuries around 2000; the bounds keep every
/// date and time far from chrono's limits.
pub const LAST_DATE: NaiveDate = NaiveDate::from_ymd_opt(3000, 12, 31).unwrap();

/// A data struct to hold the timing for all
/// prayers.
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct PrayerTimes {
    fajr: DateTime<Utc>,
    sunrise: DateTime<Utc>,
    dhuhr: DateTime<Utc>,
    asr: DateTime<Utc>,
    maghrib: DateTime<Utc>,
    isha: DateTime<Utc>,
    middle_of_the_night: DateTime<Utc>,
    qiyam: DateTime<Utc>,
    fajr_tomorrow: DateTime<Utc>,
    coordinates: Coordinates,
    date: NaiveDate,
    parameters: Parameters,
}

/// The sun's day on a date, to the second.
#[derive(Debug, Copy, Clone)]
struct SolarDay {
    date: NaiveDate,
    solar: SolarTime,
    transit: Moment,
    sunrise: Moment,
    sunset: Moment,
}

impl SolarDay {
    fn new(date: NaiveDate, coordinates: Coordinates) -> Result<SolarDay, Error> {
        let solar = SolarTime::new(date, coordinates);
        let (sunrise, sunset) = (solar.sunrise()?, solar.sunset()?);
        let at = |hours| Moment::at_hours(date, hours);
        match (at(solar.transit), at(sunrise), at(sunset)) {
            (Some(transit), Some(sunrise), Some(sunset)) => Ok(SolarDay {
                date,
                solar,
                transit,
                sunrise,
                sunset,
            }),
            // SolarTime's hours are within a day of the date.
            _ => Err(Error::DateOutOfRange(date)),
        }
    }

    /// When the sun is `depression` degrees below the horizon, before or
    /// after transit.
    fn below_horizon(&self, depression: f64, after_transit: bool) -> Option<Moment> {
        let hours = self
            .solar
            .time_for_solar_angle(Angle::new(-depression), after_transit)?;
        Moment::at_hours(self.date, hours)
    }

    fn afternoon(&self, shadow_length: f64) -> Option<Moment> {
        Moment::at_hours(self.date, self.solar.afternoon(shadow_length)?)
    }
}

/// The sun's events a day's times are made of, to the second: the day's
/// sunrise, transit and sunset, the next day's sunrise, and when the sun is
/// at the method's angles (None when it never is, or isn't asked for).
#[derive(Debug, Copy, Clone)]
struct Events {
    sunrise: Moment,
    transit: Moment,
    sunset: Moment,
    tomorrow_sunrise: Moment,
    fajr: Option<Moment>,
    isha: Option<Moment>,
    maghrib: Option<Moment>,
    asr: Option<Moment>,
}

impl Events {
    fn new(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
    ) -> Result<Events, Error> {
        let today = SolarDay::new(date, coordinates)?;
        // Of the next day only its sunrise, which ends the night: it may
        // have no sunset (the midnight sun beginning).
        let next = next_day(date)?;
        let tomorrow_sunrise = Moment::at_hours(next, SolarTime::new(next, coordinates).sunrise()?)
            .ok_or(Error::DateOutOfRange(next))?;
        let after_sunset = |angle: DepressionAngle| today.below_horizon(angle.degrees(), true);
        Ok(Events {
            sunrise: today.sunrise,
            transit: today.transit,
            sunset: today.sunset,
            tomorrow_sunrise,
            fajr: today.below_horizon(parameters.fajr_angle.degrees(), false),
            isha: match parameters.isha {
                Isha::Angle(angle) => after_sunset(angle),
                Isha::Interval(_) => None,
            },
            maghrib: parameters.maghrib_angle.and_then(after_sunset),
            asr: today.afternoon(parameters.madhab.shadow().into()),
        })
    }
}

/// A day's six times, before adjustments and rounding.
#[derive(Debug, Copy, Clone)]
struct Day {
    fajr: Moment,
    sunrise: Moment,
    dhuhr: Moment,
    asr: Moment,
    maghrib: Moment,
    isha: Moment,
}

impl Day {
    /// The times of the solar day of `date`, or, where there are none
    /// near the poles, those the polar circle resolution takes.
    fn new(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
    ) -> Result<Day, Error> {
        Day::unresolved(date, coordinates, parameters).or_else(|error| match error {
            Error::DateOutOfRange(_) => Err(error),
            Error::SunNeverRises(_)
            | Error::SunNeverSets(_)
            | Error::NoAsr(_)
            | Error::TimesOutOfOrder(_) => match parameters.polar_circle_resolution {
                PolarCircleResolution::Unresolved => None,
                PolarCircleResolution::AqrabBalad => {
                    Day::nearest_latitude(date, coordinates, parameters)
                }
                PolarCircleResolution::AqrabYaum => Day::nearest_day(date, coordinates, parameters),
            }
            .ok_or(error),
        })
    }

    /// The times of the nearest latitude toward the equator, in steps of
    /// half a degree, that has them on `date`; at the equator the season is
    /// the other hemisphere's, so the search stops there.
    fn nearest_latitude(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
    ) -> Option<Day> {
        (1..=latitude_steps(coordinates.latitude))
            .find_map(|steps| Day::at_latitude(date, coordinates, parameters, steps))
    }

    /// The times `steps` of AqrabBalad's toward the equator, if there are.
    fn at_latitude(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
        steps: u32,
    ) -> Option<Day> {
        let place = Coordinates {
            latitude: toward_equator(coordinates.latitude, steps),
            ..coordinates
        };
        Day::unresolved(date, place, parameters).ok()
    }

    /// The times of the nearest day that has them at the place, a day
    /// after then a day before, up to half a year away, moved to `date`.
    fn nearest_day(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
    ) -> Option<Day> {
        (1..=DAYS_SEARCHED)
            .flat_map(|days| [days, -days])
            .find_map(|days| Day::moved(date, coordinates, parameters, days))
    }

    /// The times `days` from `date`, if there are, moved to `date`.
    fn moved(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
        days: i64,
    ) -> Option<Day> {
        let other = date.checked_add_signed(TimeDelta::days(days))?;
        let day = Day::unresolved(other, coordinates, parameters).ok()?;
        Some(day.shifted(-days * SECONDS_PER_DAY))
    }

    /// Every time `seconds` later.
    fn shifted(self, seconds: i64) -> Day {
        let [fajr, sunrise, dhuhr, asr, maghrib, isha] = self.times().map(|t| t.plus(seconds));
        Day {
            fajr,
            sunrise,
            dhuhr,
            asr,
            maghrib,
            isha,
        }
    }

    /// The times of the solar day of `date`, which take the next day's
    /// sunrise for the length of the night.
    fn unresolved(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: &Parameters,
    ) -> Result<Day, Error> {
        let events = Events::new(date, coordinates, parameters)?;
        Day::from_events(&events, date, coordinates.latitude, parameters)
    }

    /// The rules that make the day's times of the sun's events: the
    /// high-latitude and Moonsighting Committee bounds on Fajr and Isha,
    /// Isha by interval, Tehran's Maghrib, and the order guard. Pure, and
    /// total for any events (the Kani proofs take any).
    fn from_events(
        events: &Events,
        date: NaiveDate,
        latitude: f64,
        parameters: &Parameters,
    ) -> Result<Day, Error> {
        let night = events.tomorrow_sunrise.since(events.sunset) as f64;
        let moonsighting = parameters.method == Method::MoonsightingCommittee;
        let moonsighting_high = moonsighting && latitude >= 55.0;

        let safe_fajr = if moonsighting {
            events.sunrise.plus(ops::season_adjusted_morning_twilight(
                latitude,
                date.ordinal(),
                date.year() as u32,
            ))
        } else {
            events
                .sunrise
                .plus_fraction(-parameters.night_portion(parameters.fajr_angle) * night)
        };
        let fajr = if moonsighting_high {
            Some(events.sunrise.plus_fraction(-night / 7.0))
        } else {
            events.fajr
        };
        let fajr = fajr
            .filter(|&fajr| fajr >= safe_fajr && in_order(fajr, events.sunrise))
            .unwrap_or(safe_fajr);

        let isha = match parameters.isha {
            Isha::Interval(minutes) => events.sunset.plus_minutes(minutes.into()),
            Isha::Angle(angle) => {
                let safe_isha = if moonsighting {
                    events.sunset.plus(ops::season_adjusted_evening_twilight(
                        latitude,
                        date.ordinal(),
                        date.year() as u32,
                        parameters.shafaq,
                    ))
                } else {
                    events
                        .sunset
                        .plus_fraction(parameters.night_portion(angle) * night)
                };
                let isha = if moonsighting_high {
                    Some(events.sunset.plus_fraction(night / 7.0))
                } else {
                    events.isha
                };
                isha.filter(|&isha| isha <= safe_isha && in_order(events.sunset, isha))
                    .unwrap_or(safe_isha)
            }
        };

        // Tehran's method: Maghrib when the sun is that far below the
        // horizon, if that is after sunset and clear of Isha (where Isha is
        // held to a share of a short night, the angle can crowd it, and the
        // order guard below would take every time away; Adhan's takes the
        // angle up to Isha itself).
        let maghrib = events
            .maghrib
            .filter(|&maghrib| events.sunset < maghrib && in_order(maghrib, isha))
            .unwrap_or(events.sunset);

        let day = Day {
            fajr,
            sunrise: events.sunrise,
            dhuhr: events.transit,
            asr: events.asr.ok_or(Error::NoAsr(date))?,
            maghrib,
            isha,
        };
        // Near the polar circles the times can crowd or cross; then there
        // are none (Adhan's guard).
        let times = day.times();
        match times.iter().zip(&times[1..]).all(|(&a, &b)| in_order(a, b)) {
            true => Ok(day),
            false => Err(Error::TimesOutOfOrder(date)),
        }
    }

    fn times(&self) -> [Moment; 6] {
        [
            self.fajr,
            self.sunrise,
            self.dhuhr,
            self.asr,
            self.maghrib,
            self.isha,
        ]
    }

    /// With the adjustments added and rounded.
    fn adjusted(self, parameters: &Parameters) -> Day {
        let adjust = |time: Moment, prayer| {
            time.plus_minutes(parameters.time_adjustments(prayer))
                .rounded(parameters.rounding)
        };
        Day {
            fajr: adjust(self.fajr, Prayer::Fajr),
            sunrise: adjust(self.sunrise, Prayer::Sunrise),
            dhuhr: adjust(self.dhuhr, Prayer::Dhuhr),
            asr: adjust(self.asr, Prayer::Asr),
            maghrib: adjust(self.maghrib, Prayer::Maghrib),
            isha: adjust(self.isha, Prayer::Isha),
        }
    }
}

/// AqrabBalad's step toward the equator, in degrees (Adhan's).
const LATITUDE_STEP: f64 = 0.5;
/// AqrabYaum looks this many days either way: half a year.
const DAYS_SEARCHED: i64 = 183;
const SECONDS_PER_DAY: i64 = 86_400;

/// How many of AqrabBalad's steps reach the equator from `latitude`.
fn latitude_steps(latitude: f64) -> u32 {
    (latitude.abs() / LATITUDE_STEP).ceil() as u32
}

/// `steps` of AqrabBalad's toward the equator from `latitude`, stopping
/// there.
fn toward_equator(latitude: f64, steps: u32) -> f64 {
    (latitude.abs() - f64::from(steps) * LATITUDE_STEP).max(0.0) * latitude.signum()
}

/// The middle of the night from `maghrib` to `fajr`, and the start of its
/// last third, rounded (Adhan's SunnahTimes).
/// In whole seconds, truncated toward zero: what Adhan's floating-point
/// halves and two thirds come to for any night under 2^52 seconds.
fn night_times(maghrib: Moment, fajr: Moment, rounding: Rounding) -> (Moment, Moment) {
    let night = fajr.since(maghrib);
    let middle = maghrib.plus(night / 2).rounded(rounding);
    let last_third = maghrib.plus(night * 2 / 3).rounded(rounding);
    (middle, last_third)
}

/// Of `times`, the index of the last at or before `now`.
fn last_at_or_before<T: PartialOrd>(times: &[T], now: &T) -> Option<usize> {
    times.iter().rposition(|time| time <= now)
}

/// Of `times`, the index of the first after `now`.
fn first_after<T: PartialOrd>(times: &[T], now: &T) -> Option<usize> {
    times.iter().position(|time| time > now)
}

/// The least time between one prayer time and the next (Adhan's).
const MIN_GAP_SECONDS: i64 = 5 * 60;

/// Whether `later` is at least five minutes after `earlier`.
fn in_order(earlier: Moment, later: Moment) -> bool {
    later.since(earlier) >= MIN_GAP_SECONDS
}

fn next_day(date: NaiveDate) -> Result<NaiveDate, Error> {
    date.succ_opt().ok_or(Error::DateOutOfRange(date))
}

impl PrayerTimes {
    /// The prayer times of the solar day of the UTC date `date`, or why
    /// they can't be calculated.
    pub fn new(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: Parameters,
    ) -> Result<PrayerTimes, Error> {
        if !(FIRST_DATE..=LAST_DATE).contains(&date) {
            return Err(Error::DateOutOfRange(date));
        }
        let day = Day::new(date, coordinates, &parameters)?.adjusted(&parameters);
        // The next day's Fajr, as that day's own times have it.
        let fajr_tomorrow = Day::new(next_day(date)?, coordinates, &parameters)?
            .adjusted(&parameters)
            .fajr;
        PrayerTimes::from_days(date, coordinates, parameters, day, fajr_tomorrow)
    }

    /// The times of `day` (adjusted and rounded), with the night up to
    /// `fajr_tomorrow`: its middle and the start of its last third (Adhan's
    /// SunnahTimes).
    fn from_days(
        date: NaiveDate,
        coordinates: Coordinates,
        parameters: Parameters,
        day: Day,
        fajr_tomorrow: Moment,
    ) -> Result<PrayerTimes, Error> {
        let (middle_of_the_night, qiyam) =
            night_times(day.maghrib, fajr_tomorrow, parameters.rounding);

        let utc = |time: Moment| time.utc().ok_or(Error::DateOutOfRange(date));
        Ok(PrayerTimes {
            fajr: utc(day.fajr)?,
            sunrise: utc(day.sunrise)?,
            dhuhr: utc(day.dhuhr)?,
            asr: utc(day.asr)?,
            maghrib: utc(day.maghrib)?,
            isha: utc(day.isha)?,
            middle_of_the_night: utc(middle_of_the_night)?,
            qiyam: utc(qiyam)?,
            fajr_tomorrow: utc(fajr_tomorrow)?,
            coordinates,
            date,
            parameters,
        })
    }

    /// The prayer times of the local calendar day `date` in `zone`: those
    /// of the solar day whose Dhuhr falls on `date` there.
    ///
    /// [`new`](Self::new) takes the solar day of a UTC date, whose noon is
    /// at about 12:00 local mean time on it. That is the local date's only
    /// where the zone keeps near its meridian: Kiritimati keeps UTC+14 at
    /// 157°W, so the solar day of a UTC date has its noon on the next day
    /// there.
    pub fn on_local_date<Tz: TimeZone>(
        date: NaiveDate,
        zone: &Tz,
        coordinates: Coordinates,
        parameters: Parameters,
    ) -> Result<PrayerTimes, Error> {
        let times = PrayerTimes::new(date, coordinates, parameters)?;
        let noon = times.dhuhr.with_timezone(zone).date_naive();
        match noon == date {
            true => Ok(times),
            false => {
                let solar_date = date
                    .checked_add_signed(date - noon)
                    .ok_or(Error::DateOutOfRange(date))?;
                PrayerTimes::new(solar_date, coordinates, parameters)
            }
        }
    }

    pub fn time(&self, prayer: Prayer) -> DateTime<Utc> {
        match prayer {
            Prayer::Fajr => self.fajr,
            Prayer::Sunrise => self.sunrise,
            Prayer::Dhuhr => self.dhuhr,
            Prayer::Asr => self.asr,
            Prayer::Maghrib => self.maghrib,
            Prayer::Isha => self.isha,
            Prayer::Qiyam => self.qiyam,
            Prayer::FajrTomorrow => self.fajr_tomorrow,
        }
    }

    /// The UTC date whose solar day these are the times of.
    pub fn date(&self) -> NaiveDate {
        self.date
    }

    pub fn coordinates(&self) -> Coordinates {
        self.coordinates
    }

    pub fn parameters(&self) -> Parameters {
        self.parameters
    }

    /// The middle of the night, from Maghrib to the next Fajr.
    pub fn middle_of_the_night(&self) -> DateTime<Utc> {
        self.middle_of_the_night
    }

    /// The prayer whose time has most recently come at `now`; None
    /// before this day's Fajr (the night of the day before).
    pub fn current(&self, now: DateTime<Utc>) -> Option<Prayer> {
        let index = last_at_or_before(&Prayer::IN_ORDER.map(|p| self.time(p)), &now)?;
        Prayer::IN_ORDER.get(index).copied()
    }

    /// The next prayer to come after `now`; None once tomorrow's Fajr has
    /// come (that is the next day's times).
    pub fn next(&self, now: DateTime<Utc>) -> Option<Prayer> {
        let index = first_after(&Prayer::IN_ORDER.map(|p| self.time(p)), &now)?;
        Prayer::IN_ORDER.get(index).copied()
    }

    /// The time from `now` to the next prayer, as in [`next`](Self::next).
    pub fn time_remaining(&self, now: DateTime<Utc>) -> Option<TimeDelta> {
        self.next(now).map(|prayer| self.time(prayer) - now)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;
    use crate::time::any_supported_date;

    fn any_events() -> Events {
        Events {
            sunrise: kani::any(),
            transit: kani::any(),
            sunset: kani::any(),
            tomorrow_sunrise: kani::any(),
            fajr: kani::any(),
            isha: kani::any(),
            maghrib: kani::any(),
            asr: kani::any(),
        }
    }

    #[allow(dead_code)] // Used by a stub.
    fn any_day() -> Day {
        Day {
            fajr: kani::any(),
            sunrise: kani::any(),
            dhuhr: kani::any(),
            asr: kani::any(),
            maghrib: kani::any(),
            isha: kani::any(),
        }
    }

    fn any_latitude() -> f64 {
        let latitude: f64 = kani::any();
        kani::assume((-90.0..=90.0).contains(&latitude));
        latitude
    }

    fn no_adjustments(parameters: &Parameters) -> bool {
        [
            Prayer::Fajr,
            Prayer::Sunrise,
            Prayer::Dhuhr,
            Prayer::Asr,
            Prayer::Maghrib,
            Prayer::Isha,
        ]
        .iter()
        .all(|&prayer| parameters.time_adjustments(prayer) == 0)
    }

    /// The day's rules, for any events (any moments near the supported
    /// dates, each there or not), any date, latitude and parameters (every
    /// method, madhab, high-latitude rule, rounding, angle and adjustment),
    /// never panic. A day they make has its six times in order at least
    /// five minutes apart, and, without adjustments, still in order once
    /// rounded (rounding as its proven bounds: stubbed); a day they don't
    /// is NoAsr or TimesOutOfOrder of the date.
    /// Moment::rounded, for the proof below: any moment within what
    /// time::proofs::rounding_stays_within_a_minute proves rounding moves a
    /// time by (−29 to +59 seconds, whatever the rounding).
    #[allow(dead_code)] // Used by kani::stub.
    fn any_rounding(moment: Moment, _: Rounding) -> Moment {
        let moved: i64 = kani::any();
        kani::assume((-29..=59).contains(&moved));
        moment.plus(moved)
    }

    #[kani::proof]
    #[kani::stub(Moment::rounded, any_rounding)]
    fn day_rules_are_total_and_ordered() {
        let date = any_supported_date();
        let parameters: Parameters = kani::any();
        match Day::from_events(&any_events(), date, any_latitude(), &parameters) {
            Ok(day) => {
                let times = day.times();
                for pair in times.windows(2) {
                    assert!(pair[1].since(pair[0]) >= MIN_GAP_SECONDS);
                }
                let adjusted = day.adjusted(&parameters).times();
                if no_adjustments(&parameters) {
                    for pair in adjusted.windows(2) {
                        assert!(pair[0] < pair[1]);
                    }
                }
            }
            Err(error) => {
                assert!(error == Error::NoAsr(date) || error == Error::TimesOutOfOrder(date));
            }
        }
    }

    /// The middle and last third of any night are within it, in order,
    /// rounding aside.
    #[kani::proof]
    fn night_times_are_in_order() {
        let (maghrib, fajr): (Moment, Moment) = (kani::any(), kani::any());
        kani::assume(maghrib < fajr);
        let (middle, last_third) = night_times(maghrib, fajr, kani::any());
        assert!(maghrib.plus(-60) <= middle);
        assert!(middle <= last_third);
        assert!(last_third <= fajr.plus(60));
    }

    /// For any times and any moment: current is the last that has come,
    /// next the first that hasn't, and None only when there's none.
    #[kani::proof]
    fn current_and_next_bracket_the_moment() {
        let times: [i64; 8] = kani::any();
        let now: i64 = kani::any();
        let current = last_at_or_before(&times, &now);
        let next = first_after(&times, &now);
        for (j, &time) in times.iter().enumerate() {
            match current {
                Some(i) if j == i => assert!(time <= now),
                Some(i) if j > i => assert!(time > now),
                Some(_) => {}
                None => assert!(time > now),
            }
            match next {
                Some(i) if j == i => assert!(time > now),
                Some(i) if j < i => assert!(time <= now),
                Some(_) => {}
                None => assert!(time <= now),
            }
        }
        assert!(current.is_none_or(|i| i < times.len()));
        assert!(next.is_none_or(|i| i < times.len()));
    }

    /// Moment::utc, for the proofs below: proven total by
    /// time::proofs::rounding_stays_within_a_minute, and slow to verify.
    #[allow(dead_code)] // Used by kani::stub.
    fn any_utc(_: Moment) -> Option<DateTime<Utc>> {
        Some(DateTime::UNIX_EPOCH)
    }

    /// The astronomy, for the proof below: any events, or either error it
    /// can return.
    #[allow(dead_code)] // Used by kani::stub.
    fn any_events_result(date: NaiveDate, _: Coordinates, _: &Parameters) -> Result<Events, Error> {
        match kani::any::<u8>() {
            0 => Err(Error::SunNeverRises(date)),
            1 => Err(Error::SunNeverSets(date)),
            _ => Ok(any_events()),
        }
    }

    /// PrayerTimes::new, the public calculation, for any date chrono has
    /// (supported or not), any coordinates, and any parameters without a
    /// polar circle resolution, with the astronomy any events at all and
    /// Moment::utc stubbed, never panics: it returns times or an Error, and
    /// DateOutOfRange outside the supported dates.
    #[kani::proof]
    #[kani::stub(Events::new, any_events_result)]
    #[kani::stub(Moment::utc, any_utc)]
    fn prayer_times_new_is_total() {
        let days: i32 = kani::any();
        let Some(date) = NaiveDate::from_num_days_from_ce_opt(days) else {
            return;
        };
        let mut parameters: Parameters = kani::any();
        parameters.polar_circle_resolution = PolarCircleResolution::Unresolved;
        let result = PrayerTimes::new(date, kani::any(), parameters);
        if !(FIRST_DATE..=LAST_DATE).contains(&date) {
            assert!(result == Err(Error::DateOutOfRange(date)));
        }
    }

    /// AqrabBalad's latitudes are latitudes, closer to the equator, on the
    /// same side of it.
    #[kani::proof]
    fn latitudes_toward_the_equator_are_latitudes() {
        let latitude = any_latitude();
        let steps: u32 = kani::any();
        kani::assume((1..=180).contains(&steps));
        let closer = toward_equator(latitude, steps);
        assert!(closer.abs() <= latitude.abs());
        assert!(closer * latitude >= 0.0);
        assert!(Coordinates::new(closer, 0.0).is_ok());
    }

    /// A day, for the proof below: any times, or none.
    #[allow(dead_code)] // Used by kani::stub.
    fn any_day_result(date: NaiveDate, _: Coordinates, _: &Parameters) -> Result<Day, Error> {
        match kani::any() {
            true => Ok(any_day()),
            false => Err(Error::TimesOutOfOrder(date)),
        }
    }

    /// AqrabBalad tries at most 180 latitudes, and AqrabYaum moves days by
    /// at most 183; with each day they try any times or none (stubbed),
    /// each try (every iteration of their searches) never panics. The
    /// searches around them are std's iterators over those integer ranges,
    /// so the resolutions never panic.
    #[kani::proof]
    #[kani::stub(Day::unresolved, any_day_result)]
    fn polar_circle_resolution_steps_are_total() {
        let date = any_supported_date();
        let coordinates: Coordinates = kani::any();
        let parameters: Parameters = kani::any();
        assert!(latitude_steps(coordinates.latitude) <= 180);

        let steps: u32 = kani::any();
        kani::assume((1..=180).contains(&steps));
        let _ = Day::at_latitude(date, coordinates, &parameters, steps);

        let days: i64 = kani::any();
        kani::assume(days != 0 && (-DAYS_SEARCHED..=DAYS_SEARCHED).contains(&days));
        let _ = Day::moved(date, coordinates, &parameters, days);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Configuration;
    use crate::models::high_altitude_rule::HighLatitudeRule;
    use crate::models::madhab::Madhab;
    use crate::models::rounding::Rounding;
    use chrono::{NaiveDate, TimeZone, Utc};

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    /// The six times as "H:MM" in UTC, Fajr to Isha.
    fn clock(times: &PrayerTimes) -> [String; 6] {
        [
            Prayer::Fajr,
            Prayer::Sunrise,
            Prayer::Dhuhr,
            Prayer::Asr,
            Prayer::Maghrib,
            Prayer::Isha,
        ]
        .map(|p| times.time(p).format("%-k:%M").to_string())
    }

    fn reference_day() -> PrayerTimes {
        let params = Configuration::with(Method::NorthAmerica, Madhab::Hanafi);
        PrayerTimes::new(
            date(2015, 7, 12),
            Coordinates::new(35.7750, -78.6336).unwrap(),
            params,
        )
        .unwrap()
    }

    #[test]
    fn current_prayer_through_the_day() {
        // Fajr 08:42, sunrise 10:08, Dhuhr 17:21, Asr 22:22, Maghrib 00:32,
        // Isha 01:57, the last third of the night 05:59, the next Fajr
        // 08:43 (UTC).
        let times = reference_day();
        let at = |d, h, m| Utc.with_ymd_and_hms(2015, 7, d, h, m, 0).unwrap();

        assert_eq!(times.current(at(12, 8, 0)), None);
        assert_eq!(times.current(at(12, 9, 0)), Some(Prayer::Fajr));
        assert_eq!(times.current(at(12, 11, 0)), Some(Prayer::Sunrise));
        assert_eq!(times.current(at(12, 19, 0)), Some(Prayer::Dhuhr));
        assert_eq!(times.current(at(12, 22, 26)), Some(Prayer::Asr));
        assert_eq!(times.current(at(13, 1, 0)), Some(Prayer::Maghrib));
        assert_eq!(times.current(at(13, 2, 0)), Some(Prayer::Isha));
        assert_eq!(times.current(at(13, 7, 0)), Some(Prayer::Qiyam));
        assert_eq!(times.current(at(13, 9, 0)), Some(Prayer::FajrTomorrow));
    }

    #[test]
    fn next_prayer_and_time_remaining_before_fajr_and_after_the_day() {
        // salah 0.7's next() and time_remaining() went through current(),
        // which panicked before Fajr (PR #23) and on any day but today,
        // reading the clock itself (issue #17).
        let times = reference_day();
        let at = |d, h, m| Utc.with_ymd_and_hms(2015, 7, d, h, m, 0).unwrap();

        assert_eq!(times.next(at(12, 8, 0)), Some(Prayer::Fajr));
        assert_eq!(
            times.time_remaining(at(12, 8, 0)),
            Some(TimeDelta::minutes(42))
        );
        assert_eq!(times.next(at(12, 9, 0)), Some(Prayer::Sunrise));
        assert_eq!(times.next(at(13, 2, 0)), Some(Prayer::Qiyam));
        assert_eq!(times.next(at(13, 7, 0)), Some(Prayer::FajrTomorrow));
        assert_eq!(times.next(at(13, 9, 0)), None);
        assert_eq!(times.time_remaining(at(13, 9, 0)), None);
        assert_eq!(times.current(at(20, 0, 0)), Some(Prayer::FajrTomorrow));
        assert_eq!(times.current(at(1, 0, 0)), None);
    }

    #[test]
    fn calculate_times_for_moonsighting_method() {
        let params = Configuration::with(Method::MoonsightingCommittee, Madhab::Shafi);
        let times = PrayerTimes::new(
            date(2016, 1, 31),
            Coordinates::new(35.7750, -78.6336).unwrap(),
            params,
        )
        .unwrap();

        assert_eq!(
            clock(&times),
            ["10:48", "12:16", "17:33", "20:19", "22:43", "0:05"]
        );
    }

    #[test]
    fn calculate_times_for_moonsighting_method_with_high_latitude() {
        let params = Configuration::with(Method::MoonsightingCommittee, Madhab::Hanafi);
        let times = PrayerTimes::new(
            date(2016, 1, 1),
            Coordinates::new(59.9094, 10.7349).unwrap(),
            params,
        )
        .unwrap();

        assert_eq!(
            clock(&times),
            ["6:34", "8:19", "11:25", "12:35", "14:25", "16:02"]
        );
    }

    fn seventh_of_the_night() -> Parameters {
        let mut params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        params.high_latitude_rule = HighLatitudeRule::SeventhOfTheNight;
        params.rounding = Rounding::Nearest;
        params
    }

    #[test]
    fn calculate_times_for_muslim_world_league_with_high_latitude_for_paris_france() {
        let paris = Coordinates::new(48.8566, 2.3522).unwrap();
        let times = PrayerTimes::new(date(2026, 6, 21), paris, seventh_of_the_night()).unwrap();

        assert_eq!(
            clock(&times),
            ["2:40", "3:47", "11:53", "16:10", "19:58", "21:05"]
        );
    }

    #[test]
    fn calculate_times_for_muslim_world_league_with_high_latitude_for_brussels_belgium() {
        let brussels = Coordinates::new(50.85, 4.35).unwrap();
        let times = PrayerTimes::new(date(2026, 6, 21), brussels, seventh_of_the_night()).unwrap();

        assert_eq!(
            clock(&times),
            ["2:25", "3:29", "11:45", "16:06", "20:00", "21:04"]
        );
    }

    #[test]
    fn calculate_times_with_isha_rounding_up_to_midnight() {
        // Isha in Lima on this day is at 23:59:3x UTC, which rounds up to
        // 24:00. It used to fall back to the safe Isha time (05:08 UTC).
        let params = Configuration::with(Method::NorthAmerica, Madhab::Shafi);
        let lima = Coordinates::new(-12.05, -77.04).unwrap();
        let times = PrayerTimes::new(date(2026, 4, 19), lima, params).unwrap();

        assert_eq!(
            times.time(Prayer::Isha),
            Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn sunset_rounding_up_to_midnight_rolls_over_to_the_next_day() {
        // Mexico City's sunset on this day is at 23:59:3x UTC, which rounds
        // up to 24:00 and used to panic.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let mexico_city = Coordinates::new(19.43, -99.13).unwrap();
        let times = PrayerTimes::new(date(2026, 11, 8), mexico_city, params).unwrap();

        assert_eq!(
            times.time(Prayer::Maghrib),
            Utc.with_ymd_and_hms(2026, 11, 9, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn tomorrows_fajr_is_the_next_days_fajr() {
        // salah 0.7 left it unrounded, seconds and all, unlike the next
        // day's own Fajr, and counted the last third of the night from it.
        let params = Configuration::with(Method::NorthAmerica, Madhab::Hanafi);
        let place = Coordinates::new(35.7750, -78.6336).unwrap();
        for day in 1..=27 {
            let today = PrayerTimes::new(date(2026, 2, day), place, params).unwrap();
            let tomorrow = PrayerTimes::new(date(2026, 2, day + 1), place, params).unwrap();
            assert_eq!(
                today.time(Prayer::FajrTomorrow),
                tomorrow.time(Prayer::Fajr)
            );
        }
    }

    #[test]
    fn no_rounding_keeps_the_seconds() {
        // salah 0.7 rounded every time to the minute before Rounding was
        // applied, so Rounding::None changed nothing.
        let mut params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        params.rounding = Rounding::None;
        let place = Coordinates::new(35.7750, -78.6336).unwrap();
        let times = PrayerTimes::new(date(2026, 2, 1), place, params).unwrap();
        let seconds = [
            Prayer::Fajr,
            Prayer::Sunrise,
            Prayer::Dhuhr,
            Prayer::Asr,
            Prayer::Maghrib,
        ]
        .map(|p| chrono::Timelike::second(&times.time(p)));
        assert_ne!(seconds, [0; 5]);
    }

    #[test]
    fn tehrans_maghrib_is_at_its_angle() {
        // salah 0.7 kept Tehran's 4.5° Maghrib angle but never used it:
        // Maghrib was at sunset (14:47 UTC). Adhan's times.
        let params = Configuration::with(Method::Tehran, Madhab::Shafi);
        let tehran = Coordinates::new(35.6892, 51.3890).unwrap();
        let times = PrayerTimes::new(date(2026, 3, 21), tehran, params).unwrap();

        assert_eq!(
            times.time(Prayer::Maghrib).format("%-k:%M").to_string(),
            "15:05"
        );
        assert_eq!(
            times.time(Prayer::Isha).format("%-k:%M").to_string(),
            "15:52"
        );
    }

    #[test]
    fn noon_near_the_date_line_is_on_the_date() {
        // salah 0.7 put the transit anywhere in the UTC day, so east of
        // about 176°E, when the equation of time brings noon before 00:00
        // UTC, it took the next day's: Suva's Dhuhr on 3 November (11:51
        // at UTC+12) came out on the 4th.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let suva = Coordinates::new(-18.14, 178.44).unwrap();
        let times = PrayerTimes::new(date(2026, 11, 3), suva, params).unwrap();
        assert_eq!(
            times.time(Prayer::Dhuhr),
            Utc.with_ymd_and_hms(2026, 11, 2, 23, 51, 0).unwrap()
        );

        // On each side of the date line, noon is near 12:00 local mean
        // time on the date: 00:00 UTC that day at 180°, 24:00 at -180°.
        for (longitude, offset) in [(180.0, 0), (-180.0, 24), (179.9, 0), (-179.9, 24)] {
            let place = Coordinates::new(0.0, longitude).unwrap();
            for day in [date(2026, 2, 11), date(2026, 11, 3)] {
                let dhuhr = PrayerTimes::new(day, place, params)
                    .unwrap()
                    .time(Prayer::Dhuhr);
                let mean_noon =
                    day.and_hms_opt(0, 0, 0).unwrap().and_utc() + chrono::Duration::hours(offset);
                let off = (dhuhr - mean_noon).num_minutes().abs();
                assert!(off < 30, "{longitude} on {day}: Dhuhr {dhuhr}");
            }
        }
    }

    #[test]
    fn sunset_and_twilight_converge_near_the_polar_circle() {
        // salah 0.7 took one Newton step from the 0h declination, which
        // near the end of the midnight sun is far off: in Tromsø it put
        // sunset 16 minutes early in July and Isha 34 minutes late in
        // September. Adhan's times.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let tromso = Coordinates::new(69.65, 18.96).unwrap();

        let july = PrayerTimes::new(date(2026, 7, 26), tromso, params).unwrap();
        assert_eq!(
            july.time(Prayer::Maghrib),
            Utc.with_ymd_and_hms(2026, 7, 26, 22, 13, 0).unwrap()
        );
        let september = PrayerTimes::new(date(2026, 9, 14), tromso, params).unwrap();
        assert_eq!(
            september.time(Prayer::Isha),
            Utc.with_ymd_and_hms(2026, 9, 14, 22, 6, 0).unwrap()
        );
    }

    #[test]
    fn an_asr_the_sun_cant_reach_is_an_error() {
        // Just south of the Arctic Circle at the December solstice the sun
        // rises but never gets high enough for the Asr shadow. salah 0.7
        // gave Dhuhr's time for Asr.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let jokkmokk = Coordinates::new(66.61, 19.82).unwrap();
        let day = date(2026, 12, 21);
        assert_eq!(
            PrayerTimes::new(day, jokkmokk, params),
            Err(Error::NoAsr(day))
        );
    }

    #[test]
    fn times_crowding_near_the_polar_circle_are_an_error() {
        // In Lapland at the December solstice the sun barely clears the
        // horizon: salah 0.7 gave Dhuhr at 10:39 and Asr at 10:42 UTC.
        // As in Adhan, times closer than five minutes are no times.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let lapland = Coordinates::new(66.1, 20.0).unwrap();
        let day = date(2026, 12, 21);
        assert_eq!(
            PrayerTimes::new(day, lapland, params),
            Err(Error::TimesOutOfOrder(day))
        );
    }

    #[test]
    fn moonsighting_seasons_in_the_southern_hemisphere() {
        // salah 0.7 counted the days since the southern solstice as
        // day - 172 + 365 in u32: an overflow panic before late June (in a
        // debug build), and a year too many after it. Adhan's times.
        let params = Configuration::with(Method::MoonsightingCommittee, Madhab::Shafi);
        let cape_town = Coordinates::new(-33.92, 18.42).unwrap();
        let january = PrayerTimes::new(date(2026, 1, 15), cape_town, params).unwrap();
        let september = PrayerTimes::new(date(2026, 9, 15), cape_town, params).unwrap();

        assert_eq!(
            clock(&january),
            ["2:11", "3:51", "11:01", "14:40", "18:03", "19:14"]
        );
        assert_eq!(
            clock(&september),
            ["3:23", "4:46", "10:47", "14:05", "16:41", "17:55"]
        );
    }

    #[test]
    fn polar_circle_resolutions() {
        // Issue #20, PR #21: times for the midnight sun and the polar night
        // from the nearest latitude or day that has them. Adhan's times.
        let arctic = Coordinates::new(69.65, 18.96).unwrap();
        let with = |resolution| {
            let mut params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
            params.polar_circle_resolution = resolution;
            params
        };
        let balad = with(PolarCircleResolution::AqrabBalad);
        let yaum = with(PolarCircleResolution::AqrabYaum);

        let june = PrayerTimes::new(date(2026, 6, 21), arctic, balad).unwrap();
        assert_eq!(
            clock(&june),
            ["22:46", "23:05", "10:47", "15:43", "22:26", "22:46"]
        );
        let december = PrayerTimes::new(date(2026, 12, 21), arctic, balad).unwrap();
        assert_eq!(
            clock(&december),
            ["5:15", "9:10", "10:43", "10:51", "12:15", "16:00"]
        );
        let june = PrayerTimes::new(date(2026, 6, 21), arctic, yaum).unwrap();
        assert_eq!(
            clock(&june),
            ["22:27", "23:05", "10:52", "15:38", "22:13", "22:51"]
        );
        let december = PrayerTimes::new(date(2026, 12, 21), arctic, yaum).unwrap();
        assert_eq!(
            clock(&december),
            ["4:34", "8:59", "10:31", "10:35", "12:00", "16:12"]
        );
        // Fajr and sunrise on the evening before, UTC.
        assert_eq!(june.time(Prayer::Fajr).date_naive(), date(2026, 6, 20));
    }

    #[test]
    fn times_of_a_local_date_far_from_the_zones_meridian() {
        // Kiritimati keeps UTC+14 at 157°W: the solar day of a UTC date has
        // its Dhuhr on the next local date there.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let kiritimati = Coordinates::new(1.87, -157.40).unwrap();
        let zone = chrono::FixedOffset::east_opt(14 * 3600).unwrap();
        let day = date(2026, 3, 10);
        let local =
            |times: &PrayerTimes| times.time(Prayer::Dhuhr).with_timezone(&zone).date_naive();

        let utc_day = PrayerTimes::new(day, kiritimati, params).unwrap();
        assert_eq!(local(&utc_day), date(2026, 3, 11));
        let local_day = PrayerTimes::on_local_date(day, &zone, kiritimati, params).unwrap();
        assert_eq!(local(&local_day), day);
        // Where the zone fits its meridian, it is the UTC date's.
        let mecca = Coordinates::new(21.4225, 39.8262).unwrap();
        let riyadh = chrono::FixedOffset::east_opt(3 * 3600).unwrap();
        assert_eq!(
            PrayerTimes::on_local_date(day, &riyadh, mecca, params),
            PrayerTimes::new(day, mecca, params)
        );
    }

    #[test]
    fn the_night_ends_at_the_next_sunrise_even_without_a_next_sunset() {
        // A day's times take the next day's sunrise, not its sunset: the
        // day before the midnight sun begins has times. Asking for both
        // sent AqrabBalad a step too far south for the next day, and the
        // night came out two minutes short. Adhan's times.
        let mut params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        params.polar_circle_resolution = PolarCircleResolution::AqrabBalad;
        let tromso = Coordinates::new(69.6492, 18.9553).unwrap();
        let times = PrayerTimes::new(date(2026, 6, 2), tromso, params).unwrap();
        assert_eq!(
            times
                .time(Prayer::FajrTomorrow)
                .format("%-k:%M")
                .to_string(),
            "22:52"
        );
        assert_eq!(
            times.middle_of_the_night().format("%-k:%M").to_string(),
            "22:30"
        );
    }

    #[test]
    fn tehrans_maghrib_gives_way_to_sunset_rather_than_crowd_isha() {
        // In a short summer night the 4.5° Maghrib comes within minutes of
        // an Isha held to a seventh of the night, and the day had no times.
        let mut params = Configuration::with(Method::Tehran, Madhab::Shafi);
        params.high_latitude_rule = HighLatitudeRule::SeventhOfTheNight;
        let north = Coordinates::new(58.82, 0.0).unwrap();
        let times = PrayerTimes::new(date(2026, 7, 6), north, params).unwrap();
        let gap = times.time(Prayer::Isha) - times.time(Prayer::Maghrib);
        assert!(gap >= TimeDelta::minutes(5), "{gap}");
    }

    #[test]
    fn dates_outside_the_supported_years_are_errors() {
        // salah 0.7 panicked in its date arithmetic at chrono's last date.
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let mecca = Coordinates::new(21.4225, 39.8262).unwrap();
        for date in [
            NaiveDate::MAX,
            NaiveDate::MIN,
            date(999, 12, 31),
            date(3001, 1, 1),
        ] {
            assert_eq!(
                PrayerTimes::new(date, mecca, params),
                Err(Error::DateOutOfRange(date))
            );
        }
        assert!(PrayerTimes::new(FIRST_DATE, mecca, params).is_ok());
        assert!(PrayerTimes::new(LAST_DATE, mecca, params).is_ok());
    }

    #[test]
    fn polar_day_and_night_are_errors_not_panics() {
        // Inside the Arctic Circle, issue #20. The next day's sunrise is
        // needed too, so a day before the polar night is an error as well.
        let arctic = Coordinates::new(69.65, 18.96).unwrap();
        let params = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);

        assert_eq!(
            PrayerTimes::new(date(2026, 6, 21), arctic, params),
            Err(Error::SunNeverSets(date(2026, 6, 21)))
        );
        assert_eq!(
            PrayerTimes::new(date(2026, 12, 21), arctic, params),
            Err(Error::SunNeverRises(date(2026, 12, 21)))
        );
    }
}
