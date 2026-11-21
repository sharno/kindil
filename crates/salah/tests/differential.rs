//! salah against Adhan (adhan-js, the library salah ports), case for case:
//! 29 places from 78°S to 78°N and on both sides of the date line, every
//! method, both madhabs, every high-latitude rule, shafaq and polar circle
//! resolution, through 2026 and a few other years. The cases and Adhan's
//! times are in tests/fixtures/adhan/times.csv (see generate.ts there).
//!
//! The times agree to the minute (14,961 days when written). salah differs
//! from Adhan, on purpose, where:
//!
//! - salah has no times because the next day has none: salah's
//!   PrayerTimes include the next Fajr and the night, Adhan's don't (15
//!   days here);
//! - at longitude -180° exactly, Adhan's noon is a day early (not in the
//!   table);
//! - Tehran's Maghrib angle would crowd an Isha held to a share of a short
//!   night: salah's Maghrib is at sunset, Adhan has no times (not in the
//!   table: Tehran's cases use the middle of the night).

use std::collections::HashMap;

use salah::prelude::*;

const TIMES: &str = include_str!("fixtures/adhan/times.csv");

/// Minutes the times may differ by.
const TOLERANCE_MINUTES: i64 = 0;

fn method(name: &str) -> Method {
    match name {
        "MuslimWorldLeague" => Method::MuslimWorldLeague,
        "Egyptian" => Method::Egyptian,
        "Karachi" => Method::Karachi,
        "UmmAlQura" => Method::UmmAlQura,
        "Dubai" => Method::Dubai,
        "MoonsightingCommittee" => Method::MoonsightingCommittee,
        "NorthAmerica" => Method::NorthAmerica,
        "Kuwait" => Method::Kuwait,
        "Qatar" => Method::Qatar,
        "Singapore" => Method::Singapore,
        "Tehran" => Method::Tehran,
        "Turkey" => Method::Turkey,
        other => panic!("unknown method {other}"),
    }
}

#[test]
fn times_agree_with_adhan() {
    let mut places = HashMap::new();
    let mut compared = 0;
    let mut neither = 0;
    let mut next_day_missing = 0;
    for line in TIMES.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f[0] == "place" {
            places.insert(
                f[1],
                Coordinates::new(f[2].parse().unwrap(), f[3].parse().unwrap()).unwrap(),
            );
            continue;
        }
        let place = places[f[0]];
        let date = NaiveDate::from_ymd_opt(
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].parse().unwrap(),
        )
        .unwrap();
        let madhab = match f[5] {
            "shafi" => Madhab::Shafi,
            _ => Madhab::Hanafi,
        };
        let mut parameters = Configuration::with(method(f[4]), madhab);
        parameters.high_latitude_rule = match f[6] {
            "middleofthenight" => HighLatitudeRule::MiddleOfTheNight,
            "seventhofthenight" => HighLatitudeRule::SeventhOfTheNight,
            _ => HighLatitudeRule::TwilightAngle,
        };
        parameters.shafaq = match f[7] {
            "general" => Shafaq::General,
            "ahmer" => Shafaq::Ahmer,
            _ => Shafaq::Abyad,
        };
        parameters.polar_circle_resolution = match f[8] {
            "AqrabBalad" => PolarCircleResolution::AqrabBalad,
            "AqrabYaum" => PolarCircleResolution::AqrabYaum,
            _ => PolarCircleResolution::Unresolved,
        };
        let adhan: Vec<Option<i64>> = f[9..17].iter().map(|m| m.parse().ok()).collect();
        let case = &line[..line.len() - f[9..].join(",").len() - 1];

        match (PrayerTimes::new(date, place, parameters), adhan[0]) {
            (Ok(times), Some(_)) => {
                let midnight = date.and_hms_opt(0, 0, 0).unwrap().and_utc();
                let ours = [
                    Prayer::Fajr,
                    Prayer::Sunrise,
                    Prayer::Dhuhr,
                    Prayer::Asr,
                    Prayer::Maghrib,
                    Prayer::Isha,
                ]
                .map(|p| times.time(p))
                .into_iter()
                .chain([times.middle_of_the_night(), times.time(Prayer::Qiyam)])
                .map(|t| (t - midnight).num_minutes());
                for ((name, ours), theirs) in [
                    "fajr",
                    "sunrise",
                    "dhuhr",
                    "asr",
                    "maghrib",
                    "isha",
                    "middle",
                    "last third",
                ]
                .into_iter()
                .zip(ours)
                .zip(&adhan)
                {
                    let theirs = theirs.unwrap();
                    assert!(
                        (ours - theirs).abs() <= TOLERANCE_MINUTES,
                        "{case}: {name} {ours} min, Adhan's {theirs}"
                    );
                }
                compared += 1;
            }
            (Err(_), None) => neither += 1,
            // The next day has no times: its Fajr is part of salah's.
            (Err(error), Some(_)) => {
                let next = date.succ_opt().unwrap();
                let next_has_none = match error {
                    Error::SunNeverRises(d)
                    | Error::SunNeverSets(d)
                    | Error::NoAsr(d)
                    | Error::TimesOutOfOrder(d) => d >= next,
                    Error::DateOutOfRange(_) => false,
                };
                assert!(next_has_none, "{case}: {error}, Adhan has times");
                next_day_missing += 1;
            }
            (Ok(_), None) => panic!("{case}: salah has times, Adhan none"),
        }
    }
    println!(
        "{compared} days agree to the minute; {neither} have no times in either; \
         {next_day_missing} have none in salah for want of the next day's"
    );
    assert!(compared > 10_000);
}
