//! The float layer's end: the sun's events of a date at a place, from the
//! astronomy (f64 hours after 00:00 UTC) to the verified core's whole
//! seconds. Every number the core gets from floating point passes through
//! one of three conversions here, each proven by Kani to meet the core's
//! preconditions: `seconds` (events), the night bound's seasons
//! (astronomy::ops), and the angles' shares (exact integers).

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;

use crate::astronomy::ops;
use crate::astronomy::solar::SolarTime;
use crate::astronomy::unit::Angle;
use crate::geo::Coordinates;
use crate::parameters::{HighLatitudeRule, Isha, Maghrib, NightBound, Parameters};
use crate::verified::{self, Bound, Civil, EVENT_BOUND, Events, IshaBy, MaghribBy};

/// Whole seconds from 00:00 UTC, `hours` after it (floored to the second,
/// as Adhan's TimeComponents); None for no time (NaN: the sun never gets
/// there) or one beyond the core's EVENT_BOUND.
pub(crate) fn seconds(hours: f64) -> Option<i64> {
    let seconds = (hours * 3600.0).floor();
    let bound = EVENT_BOUND as f64;
    (-bound..=bound)
        .contains(&seconds)
        .then_some(seconds as i64)
}

/// The sun on a date at a place: the float astronomy done.
pub(crate) struct Observed {
    pub civil: Civil,
    pub events: Events,
    pub bound: Bound,
    pub isha_by: IshaBy,
    /// Where the sun doesn't cross the horizon: below it all day.
    pub polar_night: bool,
}

impl Observed {
    /// The events of `civil` at `coordinates`, by `parameters`: the next
    /// day's sunrise ends the night.
    pub fn new(civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Observed {
        let today = SolarTime::new(civil.number(), coordinates);
        // Of the next day only its sunrise, counted from this day's
        // midnight: 24 hours later.
        let next_sunrise = civil
            .next()
            .and_then(|next| SolarTime::new(next.number(), coordinates).sunrise())
            .and_then(|hours| seconds(hours + 24.0));
        let below = |angle: f64, after_transit| {
            today
                .time_for_solar_angle(Angle::new(-angle), after_transit)
                .and_then(seconds)
        };
        let events = Events {
            sunrise: today.sunrise().and_then(seconds),
            transit: transit(&today, coordinates),
            sunset: today.sunset().and_then(seconds),
            next_sunrise,
            fajr: below(parameters.fajr().degrees(), false),
            isha: match parameters.isha() {
                Isha::Angle(angle) => below(angle.degrees(), true),
                Isha::Interval(_) => None,
            },
            maghrib: match parameters.maghrib() {
                Maghrib::Sunset => MaghribBy::Sunset,
                Maghrib::Angle(angle) => MaghribBy::Angle(below(angle.degrees(), true)),
            },
            asr: today
                .afternoon(parameters.madhab().shadow_length())
                .and_then(seconds),
        };
        Observed {
            civil,
            events,
            bound: bound(civil, coordinates, parameters),
            isha_by: match parameters.isha() {
                Isha::Angle(_) => IshaBy::Angle,
                Isha::Interval(minutes) => IshaBy::Interval(i64::from(minutes.minutes())),
            },
            polar_night: today.below_horizon_at_noon(),
        }
    }
}

/// The transit, in seconds: always a time near the date (Kani:
/// `transit_is_near`). Its stand-in, local mean noon, is never reached.
fn transit(today: &SolarTime, coordinates: Coordinates) -> i64 {
    match seconds(today.transit) {
        Some(transit) => transit,
        None => seconds(12.0 - coordinates.longitude.degrees() / 15.0).unwrap_or(0),
    }
}

/// How the night bounds Fajr and Isha: by a share (the high-latitude rule:
/// exact fractions), or by the Moonsighting Committee's seasons (whole
/// seconds within the core's SEASON_BOUND: Kani, astronomy::ops).
fn bound(civil: Civil, coordinates: Coordinates, parameters: &Parameters) -> Bound {
    match parameters.night_bound() {
        NightBound::HighLatitude(rule) => Bound::Share {
            fajr_share: rule.share(parameters.fajr()),
            isha_share: match parameters.isha() {
                Isha::Angle(angle) => rule.share(angle),
                // Isha by interval has no bound: any share does.
                Isha::Interval(_) => HighLatitudeRule::MiddleOfTheNight.share(parameters.fajr()),
            },
        },
        NightBound::MoonsightingSeasons(shafaq) => {
            let latitude = coordinates.latitude.degrees();
            let dyy =
                verified::days_since_solstice(civil.day_of_year(), civil.year(), latitude >= 0.0);
            Bound::Seasons {
                fajr_season: ops::season_adjusted_morning_twilight(latitude, dyy),
                isha_season: ops::season_adjusted_evening_twilight(latitude, dyy, shafaq),
                // Adhan's rule, north of 55° only.
                seventh: latitude >= 55.0,
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any f64 hours: no time, or whole seconds within the core's
    /// EVENT_BOUND.
    #[kani::proof]
    fn seconds_are_within_the_event_bound() {
        let hours: f64 = kani::any();
        if let Some(s) = seconds(hours) {
            assert!((-EVENT_BOUND..=EVENT_BOUND).contains(&s));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_are_floored_to_the_second() {
        assert_eq!(seconds(24.0 + 4.0 / 3600.9), Some(86_403));
        assert_eq!(seconds(-0.5 / 3600.0), Some(-1));
        assert_eq!(seconds(f64::NAN), None);
        assert_eq!(seconds(f64::INFINITY), None);
        assert_eq!(seconds(1e300), None);
        assert_eq!(seconds(97.0), None);
    }
}
