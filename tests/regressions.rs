//! A test per fixed bug (each failed before its fix: see the git log), and
//! the reference times salah and Adhan were checked against.

use jiff::civil::{Date, date};
use jiff::tz::{Offset, TimeZone};
use jiff::{SignedDuration, Timestamp};
use tawqit::*;

fn day(year: i16, month: i8, d: i8) -> PrayerDate {
    PrayerDate::new(date(year, month, d)).unwrap()
}

fn place(latitude: f64, longitude: f64) -> Coordinates {
    Coordinates::from_degrees(latitude, longitude).unwrap()
}

fn complete(day: Day) -> Schedule {
    match day {
        Day::Complete(schedule) => schedule,
        Day::Partial(partial) => panic!("not every time: {partial:?}"),
    }
}

fn partial(day: Day) -> PartialDay {
    match day {
        Day::Partial(partial) => partial,
        Day::Complete(schedule) => panic!("every time: {schedule:?}"),
    }
}

fn hm(at: Timestamp, zone: &TimeZone) -> String {
    let zoned = at.to_zoned_datetime(zone.clone());
    format!("{}:{:02}", zoned.hour(), zoned.minute())
}

/// The six times as "H:MM" in `zone`, Fajr to Isha.
fn clock_in(schedule: &Schedule, zone: &TimeZone) -> [String; 6] {
    Prayer::ALL.map(|p| hm(schedule.at(p), zone))
}

fn clock(schedule: &Schedule) -> [String; 6] {
    clock_in(schedule, &TimeZone::UTC)
}

fn utc(year: i16, month: i8, d: i8, hour: i8, minute: i8) -> Timestamp {
    date(year, month, d)
        .at(hour, minute, 0, 0)
        .to_zoned_datetime(TimeZone::UTC)
        .unwrap()
        .timestamp()
}

fn zone(hours: i8) -> TimeZone {
    TimeZone::fixed(Offset::constant(hours))
}

fn method(method: Method) -> Parameters {
    method.parameters()
}

/// Adhan's reference place and day for its tests.
fn reference_day() -> Schedule {
    let parameters = method(Method::NorthAmerica).with_madhab(Madhab::Hanafi);
    complete(Day::calculate(
        day(2015, 7, 12),
        place(35.7750, -78.6336),
        &parameters,
    ))
}

#[test]
fn reference_times() {
    assert_eq!(
        clock(&reference_day()),
        ["8:42", "10:08", "17:21", "22:22", "0:32", "1:57"]
    );
}

#[test]
fn the_night() {
    // Maghrib 00:32, the next Fajr 08:43: the middle at 04:38, the last
    // third from 05:59. salah 0.7 left the next Fajr unrounded.
    let schedule = reference_day();
    let Night::Known {
        middle,
        last_third,
        next_fajr,
    } = schedule.night()
    else {
        panic!("a known night")
    };
    let utc = TimeZone::UTC;
    assert_eq!(hm(middle, &utc), "4:38");
    assert_eq!(hm(last_third, &utc), "5:59");
    assert_eq!(hm(next_fajr.at(), &utc), "8:43");
}

#[test]
fn current_mark_through_the_day() {
    let schedule = reference_day();
    let at = |d, h, m| utc(2015, 7, d, h, m);
    let prayer = |p| Some(Mark::Prayer(p));

    assert_eq!(schedule.current(at(12, 8, 0)), None);
    assert_eq!(schedule.current(at(12, 9, 0)), prayer(Prayer::Fajr));
    assert_eq!(schedule.current(at(12, 11, 0)), prayer(Prayer::Sunrise));
    assert_eq!(schedule.current(at(12, 19, 0)), prayer(Prayer::Dhuhr));
    assert_eq!(schedule.current(at(12, 22, 26)), prayer(Prayer::Asr));
    assert_eq!(schedule.current(at(13, 1, 0)), prayer(Prayer::Maghrib));
    assert_eq!(schedule.current(at(13, 2, 0)), prayer(Prayer::Isha));
    assert_eq!(schedule.current(at(13, 5, 0)), Some(Mark::MiddleOfTheNight));
    assert_eq!(
        schedule.current(at(13, 7, 0)),
        Some(Mark::LastThirdOfTheNight)
    );
    assert_eq!(schedule.current(at(13, 9, 0)), Some(Mark::NextFajr));
}

#[test]
fn next_mark_before_fajr_and_after_the_day() {
    // salah 0.7's next() and time_remaining() went through current(),
    // which panicked before Fajr (PR #23) and on any day but today,
    // reading the clock itself (issue #17).
    let schedule = reference_day();
    let at = |d, h, m| utc(2015, 7, d, h, m);

    assert_eq!(
        schedule.next(at(12, 8, 0)),
        Some(Mark::Prayer(Prayer::Fajr))
    );
    assert_eq!(
        schedule.until_next(at(12, 8, 0)),
        Some(SignedDuration::from_mins(42))
    );
    assert_eq!(
        schedule.next(at(12, 9, 0)),
        Some(Mark::Prayer(Prayer::Sunrise))
    );
    assert_eq!(schedule.next(at(13, 2, 0)), Some(Mark::MiddleOfTheNight));
    assert_eq!(schedule.next(at(13, 7, 0)), Some(Mark::NextFajr));
    assert_eq!(schedule.next(at(13, 9, 0)), None);
    assert_eq!(schedule.until_next(at(13, 9, 0)), None);
    assert_eq!(schedule.current(at(20, 0, 0)), Some(Mark::NextFajr));
    assert_eq!(schedule.current(at(1, 0, 0)), None);
}

#[test]
fn moonsighting_committee() {
    let parameters = method(Method::MoonsightingCommittee);
    let schedule = complete(Day::calculate(
        day(2016, 1, 31),
        place(35.7750, -78.6336),
        &parameters,
    ));
    assert_eq!(
        clock(&schedule),
        ["10:48", "12:16", "17:33", "20:19", "22:43", "0:05"]
    );
    let hanafi = parameters.with_madhab(Madhab::Hanafi);
    let oslo = complete(Day::calculate(
        day(2016, 1, 1),
        place(59.9094, 10.7349),
        &hanafi,
    ));
    assert_eq!(
        clock(&oslo),
        ["6:34", "8:19", "11:25", "12:35", "14:25", "16:02"]
    );
    // Above 55°N, a seventh of the night: the time says so.
    assert!(matches!(
        oslo.time(Prayer::Fajr),
        Time::Fallback {
            rule: Fallback::MoonsightingSeventh | Fallback::MoonsightingSeason,
            ..
        }
    ));
}

#[test]
fn seventh_of_the_night_in_june() {
    let parameters = method(Method::MuslimWorldLeague)
        .with_high_latitude_rule(HighLatitudeRule::SeventhOfTheNight);
    let paris = complete(Day::calculate(
        day(2026, 6, 21),
        place(48.8566, 2.3522),
        &parameters,
    ));
    assert_eq!(
        clock(&paris),
        ["2:40", "3:47", "11:53", "16:10", "19:58", "21:05"]
    );
    let brussels = complete(Day::calculate(
        day(2026, 6, 21),
        place(50.85, 4.35),
        &parameters,
    ));
    assert_eq!(
        clock(&brussels),
        ["2:25", "3:29", "11:45", "16:06", "20:00", "21:04"]
    );
}

#[test]
fn a_twilight_never_reached_says_so() {
    // In Amsterdam the sun stays above -18° in June: salah 0.7 panicked,
    // then fell back silently. Now the time says which rule gave it.
    let parameters = method(Method::MuslimWorldLeague);
    let schedule = complete(Day::calculate(
        day(2026, 6, 21),
        place(52.37, 4.90),
        &parameters,
    ));
    for prayer in [Prayer::Fajr, Prayer::Isha] {
        assert!(matches!(
            schedule.time(prayer),
            Time::Fallback {
                rule: Fallback::NightShare(HighLatitudeRule::MiddleOfTheNight),
                ..
            }
        ));
    }
    assert!(matches!(schedule.time(Prayer::Dhuhr), Time::Computed(_)));
}

#[test]
fn a_time_rounding_up_to_midnight_is_the_next_day() {
    // PR #26: from 23:59:30 UTC a time rounded up to 24:00 and had no value
    // (a panic, or a silent fallback).
    let lima = complete(Day::calculate(
        day(2026, 4, 19),
        place(-12.05, -77.04),
        &method(Method::NorthAmerica),
    ));
    assert_eq!(lima.at(Prayer::Isha), utc(2026, 4, 20, 0, 0));
    let mexico_city = complete(Day::calculate(
        day(2026, 11, 8),
        place(19.43, -99.13),
        &method(Method::MuslimWorldLeague),
    ));
    assert_eq!(mexico_city.at(Prayer::Maghrib), utc(2026, 11, 9, 0, 0));
}

#[test]
fn the_next_fajr_is_the_next_days_fajr() {
    let parameters = method(Method::NorthAmerica).with_madhab(Madhab::Hanafi);
    let reference = place(35.7750, -78.6336);
    for d in 1..=27 {
        let today = complete(Day::calculate(day(2026, 2, d), reference, &parameters));
        let tomorrow = complete(Day::calculate(day(2026, 2, d + 1), reference, &parameters));
        let Night::Known { next_fajr, .. } = today.night() else {
            panic!("a known night")
        };
        assert_eq!(next_fajr, tomorrow.time(Prayer::Fajr));
    }
}

#[test]
fn rounding_off_keeps_the_seconds() {
    // salah 0.7 rounded every time to the minute before Rounding applied.
    let parameters = method(Method::MuslimWorldLeague).with_rounding(Rounding::Off);
    let schedule = complete(Day::calculate(
        day(2026, 2, 1),
        place(35.7750, -78.6336),
        &parameters,
    ));
    let seconds = Prayer::ALL.map(|p| schedule.at(p).as_second().rem_euclid(60));
    assert_ne!(seconds, [0; 6]);
}

#[test]
fn singapore_rounds_up() {
    // salah 0.7 rounded to the nearest minute, then added one: half its
    // times were a minute late.
    let parameters = method(Method::Singapore);
    let schedule = complete(Day::calculate(
        day(2021, 1, 13),
        place(1.370844612058886, 103.80145644060552),
        &parameters,
    ));
    assert_eq!(
        clock_in(&schedule, &zone(8)),
        ["5:50", "7:12", "13:15", "16:38", "19:15", "20:29"]
    );
}

#[test]
fn jakarta_with_offsets() {
    // jadwalsholat.org's times, by the Egyptian method with offsets: Adhan's.
    let minutes = |m| MinuteOffset::new(m).unwrap();
    let parameters = method(Method::Egyptian).with_method_offsets(Offsets {
        fajr: minutes(-10),
        sunrise: minutes(-2),
        dhuhr: minutes(2),
        asr: minutes(1),
        maghrib: minutes(2),
        isha: minutes(4),
    });
    let schedule = complete(Day::calculate(
        day(2021, 1, 12),
        place(-6.18233995, 106.84287154),
        &parameters,
    ));
    assert_eq!(
        clock_in(&schedule, &zone(7)),
        ["4:15", "5:45", "12:03", "15:27", "18:16", "19:31"]
    );
}

#[test]
fn tehrans_maghrib_is_at_its_angle() {
    // salah 0.7 kept the 4.5° Maghrib angle but never used it.
    let schedule = complete(Day::calculate(
        day(2026, 3, 21),
        place(35.6892, 51.3890),
        &method(Method::Tehran),
    ));
    assert_eq!(hm(schedule.at(Prayer::Maghrib), &TimeZone::UTC), "15:05");
    assert_eq!(hm(schedule.at(Prayer::Isha), &TimeZone::UTC), "15:52");
    assert!(matches!(schedule.time(Prayer::Maghrib), Time::Computed(_)));
}

#[test]
fn tehrans_maghrib_gives_way_to_sunset_rather_than_crowd_isha() {
    let parameters =
        method(Method::Tehran).with_high_latitude_rule(HighLatitudeRule::SeventhOfTheNight);
    let schedule = complete(Day::calculate(
        day(2026, 7, 6),
        place(58.82, 0.0),
        &parameters,
    ));
    let gap = schedule
        .at(Prayer::Isha)
        .duration_since(schedule.at(Prayer::Maghrib));
    assert!(gap >= SignedDuration::from_mins(5), "{gap}");
    assert!(matches!(
        schedule.time(Prayer::Maghrib),
        Time::Fallback {
            rule: Fallback::Sunset,
            ..
        }
    ));
}

#[test]
fn noon_near_the_date_line_is_on_the_date() {
    // salah 0.7 took the next day's noon east of about 176°E: Suva's Dhuhr
    // on 3 November came out on the 4th.
    let parameters = method(Method::MuslimWorldLeague);
    let suva = complete(Day::calculate(
        day(2026, 11, 3),
        place(-18.14, 178.44),
        &parameters,
    ));
    assert_eq!(suva.at(Prayer::Dhuhr), utc(2026, 11, 2, 23, 51));

    // Noon near 12:00 local mean time: 00:00 UTC at 180°, 24:00 at -180°.
    for (longitude, hours) in [(180.0, 0), (-180.0, 24), (179.9, 0), (-179.9, 24)] {
        for d in [day(2026, 2, 11), day(2026, 11, 3)] {
            let dhuhr =
                Day::calculate(d, place(0.0, longitude), &parameters).outcome(Prayer::Dhuhr);
            let Outcome::Time(dhuhr) = dhuhr else {
                panic!("a Dhuhr")
            };
            let mean_noon = d
                .date()
                .at(0, 0, 0, 0)
                .to_zoned_datetime(TimeZone::UTC)
                .unwrap()
                .timestamp()
                + SignedDuration::from_hours(hours);
            let off = dhuhr.at().duration_since(mean_noon).as_mins().abs();
            assert!(off < 30, "{longitude} on {d:?}: {dhuhr:?}");
        }
    }
}

#[test]
fn sunset_and_twilight_converge_near_the_polar_circle() {
    // One Newton step put Tromsø's sunset 16 minutes early in July and Isha
    // 34 minutes late in September. Adhan's times.
    let parameters = method(Method::MuslimWorldLeague);
    let tromso = place(69.65, 18.96);
    let july = complete(Day::calculate(day(2026, 7, 26), tromso, &parameters));
    assert_eq!(july.at(Prayer::Maghrib), utc(2026, 7, 26, 22, 13));
    let september = complete(Day::calculate(day(2026, 9, 14), tromso, &parameters));
    assert_eq!(september.at(Prayer::Isha), utc(2026, 9, 14, 22, 6));
}

#[test]
fn an_asr_the_sun_cant_reach_is_unavailable() {
    // salah 0.7 gave Dhuhr's time for Asr.
    let jokkmokk = partial(Day::calculate(
        day(2026, 12, 21),
        place(66.61, 19.82),
        &method(Method::MuslimWorldLeague),
    ));
    assert_eq!(
        jokkmokk.outcome(Prayer::Asr),
        Outcome::Unavailable(Unavailable::NoAsr)
    );
    assert!(matches!(
        jokkmokk.outcome(Prayer::Dhuhr),
        Outcome::Time(Time::Computed(_))
    ));
}

#[test]
fn times_crowding_near_the_polar_circle_are_out_of_order() {
    // salah 0.7 gave Dhuhr at 10:39 and Asr at 10:42 UTC.
    let lapland = partial(Day::calculate(
        day(2026, 12, 21),
        place(66.1, 20.0),
        &method(Method::MuslimWorldLeague),
    ));
    for prayer in Prayer::ALL {
        assert_eq!(
            lapland.outcome(prayer),
            Outcome::Unavailable(Unavailable::OutOfOrder)
        );
    }
}

#[test]
fn polar_day_and_night_are_partial_days() {
    // Issue #20: salah 0.7 panicked. Now the day says what it has (Dhuhr,
    // Asr in June) and what it hasn't, and why.
    let arctic = place(69.65, 18.96);
    let parameters = method(Method::MuslimWorldLeague);
    let june = partial(Day::calculate(day(2026, 6, 21), arctic, &parameters));
    assert_eq!(
        june.outcome(Prayer::Sunrise),
        Outcome::Unavailable(Unavailable::SunNeverSets)
    );
    assert_eq!(
        june.outcome(Prayer::Maghrib),
        Outcome::Unavailable(Unavailable::SunNeverSets)
    );
    assert!(matches!(
        june.outcome(Prayer::Dhuhr),
        Outcome::Time(Time::Computed(_))
    ));
    assert!(matches!(
        june.outcome(Prayer::Asr),
        Outcome::Time(Time::Computed(_))
    ));
    let december = partial(Day::calculate(day(2026, 12, 21), arctic, &parameters));
    assert_eq!(
        december.outcome(Prayer::Sunrise),
        Outcome::Unavailable(Unavailable::SunNeverRises)
    );
    assert_eq!(
        december.outcome(Prayer::Asr),
        Outcome::Unavailable(Unavailable::NoAsr)
    );
}

#[test]
fn moonsighting_seasons_in_the_southern_hemisphere() {
    // An overflow panic before late June, a year too many after it.
    let parameters = method(Method::MoonsightingCommittee);
    let cape_town = place(-33.92, 18.42);
    let january = complete(Day::calculate(day(2026, 1, 15), cape_town, &parameters));
    let september = complete(Day::calculate(day(2026, 9, 15), cape_town, &parameters));
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
    // Issue #20, PR #21. Adhan's times, and each says where it came from.
    let arctic = place(69.65, 18.96);
    let with =
        |resolution| method(Method::MuslimWorldLeague).with_polar_circle_resolution(resolution);
    let balad = with(PolarCircleResolution::AqrabBalad);
    let yaum = with(PolarCircleResolution::AqrabYaum);

    let june = complete(Day::calculate(day(2026, 6, 21), arctic, &balad));
    assert_eq!(
        clock(&june),
        ["22:46", "23:05", "10:47", "15:43", "22:26", "22:46"]
    );
    assert!(matches!(
        june.time(Prayer::Dhuhr),
        Time::Fallback {
            rule: Fallback::NearestLatitude(_),
            ..
        }
    ));
    let december = complete(Day::calculate(day(2026, 12, 21), arctic, &balad));
    assert_eq!(
        clock(&december),
        ["5:15", "9:10", "10:43", "10:51", "12:15", "16:00"]
    );
    let june = complete(Day::calculate(day(2026, 6, 21), arctic, &yaum));
    assert_eq!(
        clock(&june),
        ["22:27", "23:05", "10:52", "15:38", "22:13", "22:51"]
    );
    let december = complete(Day::calculate(day(2026, 12, 21), arctic, &yaum));
    assert_eq!(
        clock(&december),
        ["4:34", "8:59", "10:31", "10:35", "12:00", "16:12"]
    );
    assert!(matches!(
        june.time(Prayer::Fajr),
        Time::Fallback {
            rule: Fallback::NearestDay(_),
            ..
        }
    ));
    // Fajr and sunrise on the evening before, UTC.
    assert_eq!(
        june.at(Prayer::Fajr)
            .to_zoned_datetime(TimeZone::UTC)
            .date(),
        date(2026, 6, 20)
    );
}

#[test]
fn the_night_ends_at_the_next_sunrise_even_without_a_next_sunset() {
    // Asking for the next sunset too sent AqrabBalad a step too far south.
    let parameters = method(Method::MuslimWorldLeague)
        .with_polar_circle_resolution(PolarCircleResolution::AqrabBalad);
    let schedule = complete(Day::calculate(
        day(2026, 6, 2),
        place(69.6492, 18.9553),
        &parameters,
    ));
    let Night::Known {
        middle, next_fajr, ..
    } = schedule.night()
    else {
        panic!("a known night")
    };
    assert_eq!(hm(next_fajr.at(), &TimeZone::UTC), "22:52");
    assert_eq!(hm(middle, &TimeZone::UTC), "22:30");
}

#[test]
fn times_of_a_local_date_far_from_the_zones_meridian() {
    // Kiritimati keeps UTC+14 at 157°W.
    let parameters = method(Method::MuslimWorldLeague);
    let kiritimati = place(1.87, -157.40);
    let plus_14 = zone(14);
    let d = day(2026, 3, 10);
    let local_noon = |day: &Day| -> Date {
        let Outcome::Time(dhuhr) = day.outcome(Prayer::Dhuhr) else {
            panic!("a Dhuhr")
        };
        dhuhr.at().to_zoned_datetime(plus_14.clone()).date()
    };
    assert_eq!(
        local_noon(&Day::calculate(d, kiritimati, &parameters)),
        date(2026, 3, 11)
    );
    let local = Day::on_local_date(d, &plus_14, kiritimati, &parameters);
    assert_eq!(local_noon(&local), d.date());
    assert_eq!(local.date(), d.date());
    // Where the zone fits its meridian, it is the UTC date's.
    let makkah = place(21.4225, 39.8262);
    assert_eq!(
        Day::on_local_date(d, &zone(3), makkah, &parameters),
        Day::calculate(d, makkah, &parameters)
    );
}

#[test]
fn qiblah() {
    let paris = Qiblah::new(place(48.8566, 2.3522));
    assert!((paris.degrees() - 119.163).abs() < 0.001);
}
