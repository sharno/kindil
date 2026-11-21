//! Properties of the public API over wide ranges of inputs (proptest), with
//! the real astronomy (the Kani proofs take it as any value; VERIFICATION.md).

use jiff::civil::Date;
use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp};
use proptest::prelude::*;
use tawqit::*;

fn method() -> impl Strategy<Value = Method> {
    (0..Method::ALL.len()).prop_map(|i| Method::ALL[i])
}

fn high_latitude_rule() -> impl Strategy<Value = HighLatitudeRule> {
    prop_oneof![
        Just(HighLatitudeRule::MiddleOfTheNight),
        Just(HighLatitudeRule::SeventhOfTheNight),
        Just(HighLatitudeRule::TwilightAngle),
    ]
}

fn resolution() -> impl Strategy<Value = PolarCircleResolution> {
    prop_oneof![
        Just(PolarCircleResolution::Unresolved),
        Just(PolarCircleResolution::AqrabBalad),
        Just(PolarCircleResolution::AqrabYaum),
    ]
}

fn rounding() -> impl Strategy<Value = Rounding> {
    prop_oneof![
        Just(Rounding::Nearest),
        Just(Rounding::Up),
        Just(Rounding::Off)
    ]
}

/// A method with any madhab, high-latitude rule (but for the Moonsighting
/// Committee's seasons), polar circle resolution and rounding, and no
/// offsets.
fn parameters() -> impl Strategy<Value = Parameters> {
    (
        method(),
        any::<bool>(),
        high_latitude_rule(),
        resolution(),
        rounding(),
    )
        .prop_map(|(method, hanafi, rule, resolution, rounding)| {
            let parameters = method.parameters();
            let parameters = match parameters.night_bound() {
                NightBound::HighLatitude(_) => parameters.with_high_latitude_rule(rule),
                NightBound::MoonsightingSeasons(_) => parameters,
            };
            parameters
                .with_madhab(if hanafi {
                    Madhab::Hanafi
                } else {
                    Madhab::Shafi
                })
                .with_polar_circle_resolution(resolution)
                .with_rounding(rounding)
                .with_method_offsets(Offsets::ZERO)
        })
}

fn angle() -> impl Strategy<Value = DepressionAngle> {
    (1u16..=3000).prop_map(|h| DepressionAngle::new(f64::from(h) / 100.0).unwrap())
}

fn offsets() -> impl Strategy<Value = Offsets> {
    prop::array::uniform6(-720i16..=720).prop_map(|m| {
        let o = |i: usize| MinuteOffset::new(m[i]).unwrap();
        Offsets {
            fajr: o(0),
            sunrise: o(1),
            dhuhr: o(2),
            asr: o(3),
            maghrib: o(4),
            isha: o(5),
        }
    })
}

/// Any parameters at all: angles, intervals, Maghrib angles, offsets.
fn any_parameters() -> impl Strategy<Value = Parameters> {
    (
        parameters(),
        angle(),
        prop_oneof![
            angle().prop_map(Isha::Angle),
            (5u8..=240).prop_map(|m| Isha::Interval(IshaInterval::new(m).unwrap())),
        ],
        prop_oneof![Just(Maghrib::Sunset), angle().prop_map(Maghrib::Angle)],
        offsets(),
        offsets(),
    )
        .prop_map(|(parameters, fajr, isha, maghrib, method, user)| {
            Parameters::new(fajr, isha)
                .with_maghrib(maghrib)
                .with_night_bound(parameters.night_bound())
                .with_madhab(parameters.madhab())
                .with_polar_circle_resolution(parameters.polar_circle_resolution())
                .with_rounding(parameters.rounding())
                .with_method_offsets(method)
                .with_offsets(user)
        })
}

fn date(from: i16, to: i16) -> impl Strategy<Value = PrayerDate> {
    (from..=to, 1i16..=365).prop_map(|(year, day)| {
        let date = Date::new(year, 1, 1)
            .unwrap()
            .with()
            .day_of_year(day)
            .build()
            .unwrap();
        PrayerDate::new(date).unwrap()
    })
}

fn coordinates() -> impl Strategy<Value = Coordinates> {
    (-90.0..=90.0f64, -180.0..=180.0f64)
        .prop_map(|(lat, lon)| Coordinates::from_degrees(lat, lon).unwrap())
}

fn midnight(date: PrayerDate) -> Timestamp {
    date.date()
        .at(0, 0, 0, 0)
        .to_zoned_datetime(TimeZone::UTC)
        .unwrap()
        .timestamp()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40_000))]

    /// Any f64s, any date jiff has, any parameters, moment and zone: never
    /// a panic.
    #[test]
    fn never_panics(
        latitude in any::<f64>(),
        longitude in any::<f64>(),
        days in -4_000_000i32..4_000_000,
        parameters in any_parameters(),
        now in -300_000_000_000i64..300_000_000_000,
        offset in -25i8..=25,
    ) {
        let Ok(coordinates) = Coordinates::from_degrees(latitude, longitude) else { return Ok(()) };
        let Ok(date) = Date::new(1970, 1, 1).unwrap().checked_add(jiff::Span::new().days(days)) else {
            return Ok(())
        };
        let Ok(date) = PrayerDate::new(date) else { return Ok(()) };
        let day = Day::calculate(date, coordinates, &parameters);
        if let (Day::Complete(schedule), Ok(now)) = (day, Timestamp::from_second(now)) {
            let _ = (schedule.current(now), schedule.next(now), schedule.until_next(now));
        }
        let zone = TimeZone::fixed(Offset::constant(offset));
        let _ = Day::on_local_date(date, &zone, coordinates, &parameters);
    }

    /// A complete day without offsets: its six times in order, on whole
    /// minutes when rounded, Dhuhr within an hour of local mean noon (but
    /// for AqrabYaum), every time within a day of it. A partial day has
    /// some time missing.
    #[test]
    fn days_are_what_they_say(
        coordinates in coordinates(),
        date in date(1900, 2100),
        parameters in parameters(),
    ) {
        match Day::calculate(date, coordinates, &parameters) {
            Day::Complete(schedule) => {
                let six = Prayer::ALL.map(|p| schedule.at(p));
                for pair in six.windows(2) {
                    prop_assert!(pair[0] < pair[1], "{:?}", six);
                }
                if parameters.rounding() != Rounding::Off {
                    prop_assert!(six.iter().all(|t| t.as_second() % 60 == 0));
                }
                let mean_noon = midnight(date) + SignedDuration::from_hours(12)
                    - SignedDuration::from_secs((coordinates.longitude.degrees() * 240.0) as i64);
                if parameters.polar_circle_resolution() != PolarCircleResolution::AqrabYaum {
                    let off = schedule.at(Prayer::Dhuhr).duration_since(mean_noon);
                    prop_assert!(off.as_mins().abs() <= 60);
                }
                for time in six {
                    prop_assert!(time.duration_since(mean_noon).as_hours().abs() <= 24);
                }
            }
            Day::Partial(partial) => {
                prop_assert!(Prayer::ALL.iter().any(|&p| matches!(partial.outcome(p), Outcome::Unavailable(_))));
            }
        }
    }

    /// The night: the next Fajr is the next day's own; Maghrib, the
    /// middle, the last third and the next Fajr in order (strictly
    /// unresolved within 60°; resolved polar days can borrow from different
    /// places or days, so the next Fajr can come first, and a night of
    /// minutes rounds its marks together).
    #[test]
    fn the_night_is_in_order(
        coordinates in coordinates(),
        date in date(1900, 2099),
        parameters in parameters(),
    ) {
        let Day::Complete(schedule) = Day::calculate(date, coordinates, &parameters) else { return Ok(()) };
        let Night::Known { middle, last_third, next_fajr } = schedule.night() else { return Ok(()) };
        let next = PrayerDate::new(date.date().tomorrow().unwrap()).unwrap();
        prop_assert_eq!(Day::calculate(next, coordinates, &parameters).outcome(Prayer::Fajr), Outcome::Time(next_fajr));
        let maghrib = schedule.at(Prayer::Maghrib);
        let unresolved = parameters.polar_circle_resolution() == PolarCircleResolution::Unresolved;
        if unresolved && coordinates.latitude.degrees().abs() <= 60.0 {
            prop_assert!(maghrib < middle && middle < last_third && last_third < next_fajr.at());
        }
        if maghrib <= next_fajr.at() {
            prop_assert!(maghrib <= middle && middle <= last_third && last_third <= next_fajr.at());
        }
    }

    /// Every day within 60° is complete, and every day with AqrabBalad.
    #[test]
    fn times_exist(
        coordinates in coordinates(),
        date in date(1900, 2100),
        parameters in parameters(),
    ) {
        if coordinates.latitude.degrees().abs() <= 60.0 {
            let day = Day::calculate(date, coordinates, &parameters);
            prop_assert!(matches!(day, Day::Complete(_)), "{day:?}");
        }
        let balad = parameters.with_polar_circle_resolution(PolarCircleResolution::AqrabBalad);
        prop_assert!(matches!(Day::calculate(date, coordinates, &balad), Day::Complete(_)));
    }

    /// current is the latest mark at or before any moment, next the
    /// earliest after it, until_next the time to it.
    #[test]
    fn current_and_next_bracket_the_moment(
        coordinates in coordinates(),
        date in date(1900, 2100),
        parameters in parameters(),
        offset in -86_400i64..(3 * 86_400),
    ) {
        let Day::Complete(schedule) = Day::calculate(date, coordinates, &parameters) else { return Ok(()) };
        let now = midnight(date) + SignedDuration::from_secs(offset);
        if let Some(current) = schedule.current(now) {
            prop_assert!(schedule.mark(current).unwrap() <= now);
        }
        if let Some(next) = schedule.next(now) {
            let at = schedule.mark(next).unwrap();
            prop_assert!(at > now);
            prop_assert_eq!(schedule.until_next(now), Some(at.duration_since(now)));
        }
    }

    /// A local date's Dhuhr falls on it in the zone, for a zone within 10
    /// hours of local mean time (Kiritimati's is 24.5 hours off: within
    /// 0.5 of it, a day apart).
    #[test]
    fn local_dates_have_their_noon(
        latitude in -60.0..=60.0f64,
        longitude in -180.0..=180.0f64,
        date in date(1900, 2100),
        off_meridian in -9i8..=9,
    ) {
        let hours = match (longitude / 15.0).round() as i8 + off_meridian {
            o if o > 14 => o - 24,
            o if o < -12 => o + 24,
            o => o,
        };
        let coordinates = Coordinates::from_degrees(latitude, longitude).unwrap();
        let zone = TimeZone::fixed(Offset::constant(hours));
        let parameters = Method::MuslimWorldLeague.parameters();
        let day = Day::on_local_date(date, &zone, coordinates, &parameters);
        let Outcome::Time(dhuhr) = day.outcome(Prayer::Dhuhr) else { panic!("a Dhuhr") };
        prop_assert_eq!(zone.to_datetime(dhuhr.at()).date(), date.date());
    }

    /// Qiblah bearings are in [0, 360) everywhere.
    #[test]
    fn qiblah_is_a_bearing(coordinates in coordinates()) {
        let bearing = Qiblah::new(coordinates).degrees();
        prop_assert!((0.0..360.0).contains(&bearing), "{bearing}");
    }
}
