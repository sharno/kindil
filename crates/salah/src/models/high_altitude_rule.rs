// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::models::coordinates::Coordinates;

/// Rule for approximating Fajr and Isha at high latitudes
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum HighLatitudeRule {
    /// Fajr won't be earlier than the midpoint of the night and isha
    /// won't be later than the midpoint of the night. This is the default
    /// value to prevent fajr and isha crossing boundaries.
    MiddleOfTheNight,

    /// Fajr will never be earlier than the beginning of the last seventh of
    /// the night and Isha will never be later than the end of the first seventh of the night.
    ///
    /// This is recommended to use for locations above 48° latitude to prevent prayer
    /// times that would be difficult to perform.
    SeventhOfTheNight,

    /// The night is divided into portions of roughly 1/3. The exact value is derived
    /// by dividing the fajr/isha angles by 60.
    ///
    /// This can be used to prevent difficult fajr and isha times at certain locations.
    TwilightAngle,
}

impl HighLatitudeRule {
    /// Seventh of the night beyond 48° north or south, else the middle of
    /// the night.
    pub fn recommended(coordinates: Coordinates) -> HighLatitudeRule {
        if coordinates.latitude.abs() > 48.0 {
            HighLatitudeRule::SeventhOfTheNight
        } else {
            HighLatitudeRule::MiddleOfTheNight
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommended_rule_seventh_of_night() {
        let location = Coordinates::new(48.983226, -3.216649).unwrap();

        assert_eq!(
            HighLatitudeRule::recommended(location),
            HighLatitudeRule::SeventhOfTheNight
        );
    }

    #[test]
    fn recommended_rule_in_the_south() {
        // salah 0.7 only looked north of 48°.
        let south = Coordinates::new(-51.62, -69.22).unwrap();

        assert_eq!(
            HighLatitudeRule::recommended(south),
            HighLatitudeRule::SeventhOfTheNight
        );
    }

    #[test]
    fn recommended_rule_middle_of_night() {
        let location = Coordinates::new(45.983226, -3.216649).unwrap();

        assert_eq!(
            HighLatitudeRule::recommended(location),
            HighLatitudeRule::MiddleOfTheNight
        );
    }
}
