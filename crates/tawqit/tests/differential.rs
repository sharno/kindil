//! tawqit against Adhan (adhan-js, the library tawqit's algorithms follow),
//! case for case: 29 places from 78°S to 78°N and on both sides of the
//! date line, every method, both madhabs, every high-latitude rule, shafaq
//! and polar circle resolution, through 2026 and a few other years. The
//! cases and Adhan's times are in tests/fixtures/adhan/times.csv (see
//! generate.ts there).
//!
//! Every day Adhan has times for is a complete day here with the same six
//! times, to the minute, and every day it hasn't is a partial day; the
//! night's middle and last third agree where both know the next Fajr.
//! tawqit differs from Adhan, on purpose, where:
//!
//! - the next day has no Fajr: Adhan has no night; tawqit's night is
//!   `Night::Unknown` (here a few days: counted);
//! - at longitude -180° exactly, Adhan's noon is a day early (not in the
//!   table);
//! - Tehran's Maghrib angle would crowd an Isha held to a share of a short
//!   night: tawqit's Maghrib is at sunset, Adhan has no times (not in the
//!   table: Tehran's cases use the middle of the night).

use std::collections::HashMap;

use jiff::civil::date;
use jiff::tz::TimeZone;
use tawqit::*;

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
    let (mut compared, mut neither, mut night_unknown) = (0, 0, 0);
    for line in TIMES.lines() {
        let f: Vec<&str> = line.split(',').collect();
        if f[0] == "place" {
            let place = Coordinates::from_degrees(f[2].parse().unwrap(), f[3].parse().unwrap());
            places.insert(f[1], place.unwrap());
            continue;
        }
        let place = places[f[0]];
        let civil = date(
            f[1].parse().unwrap(),
            f[2].parse().unwrap(),
            f[3].parse().unwrap(),
        );
        let madhab = match f[5] {
            "shafi" => Madhab::Shafi,
            _ => Madhab::Hanafi,
        };
        let mut parameters = method(f[4]).parameters().with_madhab(madhab);
        parameters = match (parameters.night_bound(), f[6], f[7]) {
            (NightBound::MoonsightingSeasons(_), _, shafaq) => {
                parameters.with_night_bound(NightBound::MoonsightingSeasons(match shafaq {
                    "general" => Shafaq::General,
                    "ahmer" => Shafaq::Ahmer,
                    _ => Shafaq::Abyad,
                }))
            }
            (NightBound::HighLatitude(_), rule, _) => {
                parameters.with_high_latitude_rule(match rule {
                    "middleofthenight" => HighLatitudeRule::MiddleOfTheNight,
                    "seventhofthenight" => HighLatitudeRule::SeventhOfTheNight,
                    _ => HighLatitudeRule::TwilightAngle,
                })
            }
        };
        parameters = parameters.with_polar_circle_resolution(match f[8] {
            "AqrabBalad" => PolarCircleResolution::AqrabBalad,
            "AqrabYaum" => PolarCircleResolution::AqrabYaum,
            _ => PolarCircleResolution::Unresolved,
        });
        let adhan: Vec<Option<i64>> = f[9..17].iter().map(|m| m.parse().ok()).collect();
        let case = &line[..line.len() - f[9..].join(",").len() - 1];
        let midnight = civil
            .at(0, 0, 0, 0)
            .to_zoned_datetime(TimeZone::UTC)
            .unwrap()
            .timestamp();
        let minutes = |t: jiff::Timestamp| t.duration_since(midnight).as_secs().div_euclid(60);

        match (
            Day::calculate(PrayerDate::new(civil).unwrap(), place, &parameters),
            adhan[0],
        ) {
            (Day::Complete(schedule), Some(_)) => {
                let ours = Prayer::ALL.map(|p| minutes(schedule.at(p)));
                for ((prayer, ours), theirs) in Prayer::ALL.iter().zip(ours).zip(&adhan) {
                    let theirs = theirs.unwrap();
                    assert!(
                        (ours - theirs).abs() <= TOLERANCE_MINUTES,
                        "{case}: {prayer:?} {ours} min, Adhan's {theirs}"
                    );
                }
                match (schedule.night(), adhan[6], adhan[7]) {
                    (
                        Night::Known {
                            middle, last_third, ..
                        },
                        Some(m),
                        Some(l),
                    ) => {
                        assert!(
                            (minutes(middle) - m).abs() <= TOLERANCE_MINUTES,
                            "{case}: middle {} vs {m}",
                            minutes(middle)
                        );
                        assert!(
                            (minutes(last_third) - l).abs() <= TOLERANCE_MINUTES,
                            "{case}: last third"
                        );
                    }
                    (Night::Unknown(_), None, None) => night_unknown += 1,
                    (night, m, l) => panic!("{case}: night {night:?}, Adhan's {m:?} {l:?}"),
                }
                compared += 1;
            }
            (Day::Partial(_), None) => neither += 1,
            (Day::Partial(partial), Some(_)) => {
                panic!("{case}: Adhan has times, tawqit {partial:?}")
            }
            (Day::Complete(_), None) => panic!("{case}: tawqit has times, Adhan none"),
        }
    }
    println!(
        "{compared} days agree to the minute ({night_unknown} with no next Fajr in either); \
         {neither} have no times in either"
    );
    assert!(compared > 10_000);
}
