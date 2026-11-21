//! tawqit's astronomy against an independent one: NOAA's solar calculator
//! (the equations of its spreadsheet, after Meeus, by way of the equation
//! of time rather than salah's right ascension and sidereal time),
//! written here from those equations.
//!
//! Sunrise, transit and sunset agree within 10 seconds (3 at worst when
//! written) from 65°S to 65°N, and the 18° twilights from 45°S to 45°N,
//! over two centuries of dates, away from the edge of the sun's reaching an
//! angle at all.

use jiff::Timestamp;
use jiff::tz::TimeZone;
use tawqit::*;

const TOLERANCE_SECONDS: i64 = 10;

fn radians(degrees: f64) -> f64 {
    degrees.to_radians()
}

fn degrees(radians: f64) -> f64 {
    radians.to_degrees()
}

/// The sun's declination (degrees) and the equation of time (minutes) at
/// the Julian day `jd`, from NOAA's spreadsheet.
fn sun(jd: f64) -> (f64, f64) {
    let t = (jd - 2451545.0) / 36525.0;
    let l0 = (280.46646 + t * (36000.76983 + t * 0.0003032)).rem_euclid(360.0);
    let m = 357.52911 + t * (35999.05029 - 0.0001537 * t);
    let e = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);
    let c = radians(m).sin() * (1.914602 - t * (0.004817 + 0.000014 * t))
        + radians(2.0 * m).sin() * (0.019993 - 0.000101 * t)
        + radians(3.0 * m).sin() * 0.000289;
    let true_longitude = l0 + c;
    let omega = 125.04 - 1934.136 * t;
    let apparent_longitude = true_longitude - 0.00569 - 0.00478 * radians(omega).sin();
    let mean_obliquity =
        23.0 + (26.0 + (21.448 - t * (46.815 + t * (0.00059 - t * 0.001813))) / 60.0) / 60.0;
    let obliquity = mean_obliquity + 0.00256 * radians(omega).cos();
    let declination =
        degrees((radians(obliquity).sin() * radians(apparent_longitude).sin()).asin());
    let y = (radians(obliquity / 2.0)).tan().powi(2);
    let equation_of_time = 4.0
        * degrees(
            y * (2.0 * radians(l0)).sin() - 2.0 * e * radians(m).sin()
                + 4.0 * e * y * radians(m).sin() * (2.0 * radians(l0)).cos()
                - 0.5 * y * y * (4.0 * radians(l0)).sin()
                - 1.25 * e * e * (2.0 * radians(m)).sin(),
        );
    (declination, equation_of_time)
}

/// Minutes after 00:00 UTC on the day of `jd0` (its midnight) when the
/// sun's center is `altitude` degrees up, rising or setting (`side` -1 or
/// 1), or at transit (`side` 0); recomputed at the event's own time until
/// it settles. None if the sun doesn't get there.
fn event(jd0: f64, latitude: f64, longitude: f64, altitude: f64, side: f64) -> Option<f64> {
    let mut minutes = 720.0 - 4.0 * longitude;
    for _ in 0..5 {
        let (declination, equation_of_time) = sun(jd0 + minutes / 1440.0);
        let noon = 720.0 - 4.0 * longitude - equation_of_time;
        if side == 0.0 {
            minutes = noon;
            continue;
        }
        let cos_h = (radians(altitude).sin()
            - radians(latitude).sin() * radians(declination).sin())
            / (radians(latitude).cos() * radians(declination).cos());
        if !(-1.0..=1.0).contains(&cos_h) {
            return None;
        }
        minutes = noon + side * 4.0 * degrees(cos_h.acos());
    }
    Some(minutes)
}

fn seconds_off(time: Timestamp, midnight: Timestamp, minutes: f64) -> i64 {
    (time.duration_since(midnight).as_secs_f64() - minutes * 60.0).round() as i64
}

#[test]
fn sun_and_twilights_agree_with_noaa() {
    // No adjustments, no rounding, 18° twilights.
    let eighteen = DepressionAngle::new(18.0).unwrap();
    let parameters = Parameters::new(eighteen, Isha::Angle(eighteen)).with_rounding(Rounding::Off);

    let mut worst = [0i64; 5];
    let mut compared = 0;
    for year in (1950i16..=2150).step_by(25) {
        for day in (1..=365).step_by(11) {
            let date = jiff::civil::Date::new(year, 1, 1)
                .unwrap()
                .with()
                .day_of_year(day)
                .build()
                .unwrap();
            let midnight = date
                .at(0, 0, 0, 0)
                .to_zoned_datetime(TimeZone::UTC)
                .unwrap()
                .timestamp();
            let jd0 = 2440587.5 + midnight.as_second() as f64 / 86400.0;
            for latitude in (-65..=65).step_by(5).map(f64::from) {
                for longitude in (-180..180).step_by(30).map(f64::from) {
                    let place = Coordinates::from_degrees(latitude, longitude).unwrap();
                    let times = Day::calculate(PrayerDate::new(date).unwrap(), place, &parameters);
                    let horizon = -50.0 / 60.0;
                    // Events near the edge of not happening are ill-conditioned
                    // (the hour angle's slope is unbounded there): skip within
                    // 2° of it.
                    let robust = |altitude: f64| {
                        let (declination, _) = sun(jd0 + 0.5);
                        let cos_h = (radians(altitude).sin()
                            - radians(latitude).sin() * radians(declination).sin())
                            / (radians(latitude).cos() * radians(declination).cos());
                        cos_h.abs() < radians(88.0).sin()
                    };
                    let mut check = |slot: usize, prayer: Prayer, altitude: f64, side: f64| {
                        if side != 0.0 && !robust(altitude) {
                            return;
                        }
                        let Some(minutes) = event(jd0, latitude, longitude, altitude, side) else {
                            return;
                        };
                        // The sun's own time, where the day has it.
                        let Outcome::Time(Time::Computed(at)) = times.outcome(prayer) else {
                            return;
                        };
                        let off = seconds_off(at, midnight, minutes);
                        assert!(
                            off.abs() <= TOLERANCE_SECONDS,
                            "{prayer:?} at {latitude}, {longitude} on {date}: {off} s from NOAA's"
                        );
                        worst[slot] = worst[slot].max(off.abs());
                        compared += 1;
                    };
                    check(0, Prayer::Sunrise, horizon, -1.0);
                    check(1, Prayer::Dhuhr, 0.0, 0.0);
                    check(2, Prayer::Maghrib, horizon, 1.0);
                    if latitude.abs() <= 45.0 {
                        check(3, Prayer::Fajr, -18.0, -1.0);
                        check(4, Prayer::Isha, -18.0, 1.0);
                    }
                }
            }
        }
    }
    println!(
        "{compared} times; worst seconds off (sunrise, transit, sunset, Fajr, Isha): {worst:?}"
    );
    assert!(compared > 100_000);
}
