//! Properties of the public API over wide ranges of inputs (proptest).
//!
//! The Kani proofs (VERIFICATION.md) cover every input, but with the
//! astronomy taken as any value; these run the real astronomy.

use chrono::FixedOffset;
use proptest::prelude::*;
use salah::prelude::*;

const SIX: [Prayer; 6] = [
    Prayer::Fajr,
    Prayer::Sunrise,
    Prayer::Dhuhr,
    Prayer::Asr,
    Prayer::Maghrib,
    Prayer::Isha,
];

/// Every method but Other, whose 0° angles are placeholders.
fn method() -> impl Strategy<Value = Method> {
    prop_oneof![
        Just(Method::MuslimWorldLeague),
        Just(Method::Egyptian),
        Just(Method::Karachi),
        Just(Method::UmmAlQura),
        Just(Method::Dubai),
        Just(Method::MoonsightingCommittee),
        Just(Method::NorthAmerica),
        Just(Method::Kuwait),
        Just(Method::Qatar),
        Just(Method::Singapore),
        Just(Method::Tehran),
        Just(Method::Turkey),
    ]
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
        Just(Rounding::None)
    ]
}

/// A method with any madhab, high-latitude rule, polar circle resolution
/// and rounding, and no adjustments.
fn parameters() -> impl Strategy<Value = Parameters> {
    (
        method(),
        any::<bool>(),
        high_latitude_rule(),
        resolution(),
        rounding(),
    )
        .prop_map(|(method, hanafi, rule, resolution, rounding)| {
            let madhab = if hanafi {
                Madhab::Hanafi
            } else {
                Madhab::Shafi
            };
            let mut parameters = Configuration::with(method, madhab);
            parameters.high_latitude_rule = rule;
            parameters.polar_circle_resolution = resolution;
            parameters.rounding = rounding;
            parameters.method_adjustments = TimeAdjustment::default();
            parameters
        })
}

/// Any parameters at all: angles, Isha by interval, adjustments.
fn any_parameters() -> impl Strategy<Value = Parameters> {
    (
        (parameters(), any::<bool>()).prop_map(|(mut parameters, other)| {
            if other {
                parameters.method = Method::Other;
            }
            parameters
        }),
        0.0..=30.0f64,
        prop::option::of(0.0..=30.0f64),
        prop_oneof![
            (0.0..=30.0f64).prop_map(|a| Isha::Angle(DepressionAngle::new(a).unwrap())),
            any::<u8>().prop_map(Isha::Interval),
        ],
        prop::array::uniform6(any::<i16>()),
    )
        .prop_map(|(mut parameters, fajr, maghrib, isha, a)| {
            parameters.fajr_angle = DepressionAngle::new(fajr).unwrap();
            parameters.maghrib_angle = maghrib.map(|m| DepressionAngle::new(m).unwrap());
            parameters.isha = isha;
            parameters.adjustments = TimeAdjustment::new(a[0], a[1], a[2], a[3], a[4], a[5]);
            parameters
        })
}

fn date(from: i32, to: i32) -> impl Strategy<Value = NaiveDate> {
    (from..=to, 1u32..=366).prop_map(|(year, day)| {
        NaiveDate::from_yo_opt(year, day)
            .unwrap_or_else(|| NaiveDate::from_yo_opt(year, 365).unwrap())
    })
}

fn coordinates() -> impl Strategy<Value = Coordinates> {
    (-90.0..=90.0f64, -180.0..=180.0f64).prop_map(|(lat, lon)| Coordinates::new(lat, lon).unwrap())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(40_000))]

    /// Any f64s, any date chrono has, any parameters: never a panic.
    #[test]
    fn never_panics(
        latitude in any::<f64>(),
        longitude in any::<f64>(),
        days in any::<i32>(),
        parameters in any_parameters(),
        now in any::<i64>(),
    ) {
        let Ok(coordinates) = Coordinates::new(latitude, longitude) else { return Ok(()) };
        let Some(date) = NaiveDate::from_num_days_from_ce_opt(days) else { return Ok(()) };
        if let Ok(times) = PrayerTimes::new(date, coordinates, parameters) {
            if let Some(now) = DateTime::from_timestamp(now % 100_000_000_000, 0) {
                let _ = (times.current(now), times.next(now), times.time_remaining(now));
            }
        }
        let zone = FixedOffset::east_opt((now % 50_400) as i32).unwrap();
        let _ = PrayerTimes::on_local_date(date, &zone, coordinates, parameters);
    }

    /// Without adjustments, a day's six times are in order, on whole minutes
    /// when rounded, Dhuhr is within an hour of local mean noon, and the
    /// whole day within a day of it.
    #[test]
    fn times_are_ordered_and_near_the_date(
        coordinates in coordinates(),
        date in date(1900, 2100),
        parameters in parameters(),
    ) {
        let Ok(times) = PrayerTimes::new(date, coordinates, parameters) else { return Ok(()) };
        let six = SIX.map(|p| times.time(p));
        for pair in six.windows(2) {
            prop_assert!(pair[0] < pair[1], "{:?}", six);
        }
        if parameters.rounding != Rounding::None {
            prop_assert!(six.iter().all(|t| t.second() == 0));
        }
        let mean_noon = date.and_hms_opt(12, 0, 0).unwrap().and_utc()
            - TimeDelta::seconds((coordinates.longitude() * 240.0) as i64);
        if parameters.polar_circle_resolution != PolarCircleResolution::AqrabYaum {
            prop_assert!((times.time(Prayer::Dhuhr) - mean_noon).num_minutes().abs() <= 60);
        }
        for time in six {
            prop_assert!((time - mean_noon).num_hours().abs() <= 24);
        }
    }

    /// The night: Maghrib, its middle, its last third and the next Fajr in
    /// order, and the next Fajr the next day's own. (A resolved polar day
    /// and the next can borrow from different places or days, so the next
    /// Fajr can come before Maghrib; unresolved within 60° it never does.)
    #[test]
    fn the_night_is_in_order(
        coordinates in coordinates(),
        date in date(1900, 2099),
        parameters in parameters(),
    ) {
        let Ok(times) = PrayerTimes::new(date, coordinates, parameters) else { return Ok(()) };
        // The next day's times need the day after's too, which may have none.
        let Ok(next) = PrayerTimes::new(date.succ_opt().unwrap(), coordinates, parameters) else {
            return Ok(());
        };
        prop_assert_eq!(times.time(Prayer::FajrTomorrow), next.time(Prayer::Fajr));
        let unresolved = parameters.polar_circle_resolution == PolarCircleResolution::Unresolved;
        if unresolved && coordinates.latitude().abs() <= 60.0 {
            prop_assert!(times.time(Prayer::Maghrib) < times.time(Prayer::FajrTomorrow));
        }
        if times.time(Prayer::FajrTomorrow) <= times.time(Prayer::Maghrib) {
            return Ok(());
        }
        // A night of minutes rounds its middle and last third together.
        prop_assert!(times.time(Prayer::Maghrib) <= times.middle_of_the_night());
        prop_assert!(times.middle_of_the_night() <= times.time(Prayer::Qiyam));
        prop_assert!(times.time(Prayer::Qiyam) <= times.time(Prayer::FajrTomorrow));
    }

    /// Away from the polar circles every day has times, and AqrabBalad
    /// finds times everywhere.
    #[test]
    fn times_exist(
        coordinates in coordinates(),
        date in date(1900, 2100),
        parameters in parameters(),
    ) {
        let result = PrayerTimes::new(date, coordinates, parameters);
        if coordinates.latitude().abs() <= 60.0 {
            prop_assert!(result.is_ok(), "{result:?}");
        }
        let mut balad = parameters;
        balad.polar_circle_resolution = PolarCircleResolution::AqrabBalad;
        prop_assert!(PrayerTimes::new(date, coordinates, balad).is_ok());
    }

    /// current() and next() bracket any moment of the day.
    #[test]
    fn current_and_next_bracket_the_moment(
        coordinates in coordinates(),
        date in date(1900, 2100),
        parameters in parameters(),
        offset in -86_400i64..(3 * 86_400),
    ) {
        let Ok(times) = PrayerTimes::new(date, coordinates, parameters) else { return Ok(()) };
        let now = date.and_hms_opt(0, 0, 0).unwrap().and_utc() + TimeDelta::seconds(offset);
        if let Some(current) = times.current(now) {
            prop_assert!(times.time(current) <= now);
        }
        if let Some(next) = times.next(now) {
            prop_assert!(times.time(next) > now);
            prop_assert_eq!(times.time_remaining(now), Some(times.time(next) - now));
        }
    }

    /// The times of a local date have their Dhuhr on that date in the zone,
    /// for a zone within 10 hours of local mean time, give or take a day
    /// (Kiritimati's is 24.5 hours off). Twelve hours off, noon is at
    /// midnight, and there may be no solar day with its noon on the date.
    #[test]
    fn local_dates_have_their_noon(
        latitude in -60.0..=60.0f64,
        longitude in -180.0..=180.0f64,
        date in date(1900, 2100),
        off_meridian in -9i32..=9,
    ) {
        let offset_hours = match (longitude / 15.0).round() as i32 + off_meridian {
            o if o > 14 => o - 24,
            o if o < -12 => o + 24,
            o => o,
        };
        let coordinates = Coordinates::new(latitude, longitude).unwrap();
        let zone = FixedOffset::east_opt(offset_hours * 3600).unwrap();
        let parameters = Configuration::with(Method::MuslimWorldLeague, Madhab::Shafi);
        let times = PrayerTimes::on_local_date(date, &zone, coordinates, parameters).unwrap();
        prop_assert_eq!(times.time(Prayer::Dhuhr).with_timezone(&zone).date_naive(), date);
    }

    /// Qiblah bearings are in [0, 360) everywhere.
    #[test]
    fn qiblah_is_a_bearing(coordinates in coordinates()) {
        let bearing = Qiblah::new(coordinates).value();
        prop_assert!((0.0..360.0).contains(&bearing), "{bearing}");
    }
}
