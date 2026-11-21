// tawqit: began from salah, by Farhan Ahmed (see LICENSE).

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;
use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::astronomy::unit::Angle;
use crate::geo::Coordinates;

#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
/// The direction of the Kaaba from a place: degrees clockwise from true
/// north, in [0, 360).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Qiblah(f64);

impl Qiblah {
    pub fn new(location_coordinates: Coordinates) -> Self {
        // Equation from "Spherical Trigonometry For the use
        // of colleges and schools" page 50
        let makkah_coordinates = Coordinates::KAABA;
        let term1 = (makkah_coordinates.longitude_angle().radians()
            - location_coordinates.longitude_angle().radians())
        .sin();
        let term2 = makkah_coordinates.latitude_angle().radians().tan()
            * location_coordinates.latitude_angle().radians().cos();
        let term3 = (makkah_coordinates.longitude_angle().radians()
            - location_coordinates.longitude_angle().radians())
        .cos()
            * location_coordinates.latitude_angle().radians().sin();
        let term4 = term1.atan2(term2 - term3);

        Qiblah(Angle::from_radians(term4).unwound().degrees)
    }

    /// Degrees clockwise from true north, in [0, 360).
    pub fn degrees(&self) -> f64 {
        self.0
    }
}

impl fmt::Display for Qiblah {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.degrees())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qiblah_direction_from_nyc_in_north_america() {
        let nyc = Coordinates::from_degrees(40.7128, -74.0059).unwrap();
        let qiblah = Qiblah::new(nyc);

        assert!((qiblah.degrees() - 58.4817635).abs() < 1e-7);
    }

    #[test]
    fn qiblah_direction_from_sf_in_north_america() {
        let sf = Coordinates::from_degrees(37.7749, -122.4194).unwrap();
        let qiblah = Qiblah::new(sf);

        assert_eq!(qiblah.degrees(), 18.843822245692426);
    }

    #[test]
    fn qiblah_direction_from_dc_in_north_america() {
        let dc = Coordinates::from_degrees(38.9072, -77.0369).unwrap();
        let qiblah = Qiblah::new(dc);

        assert_eq!(qiblah.degrees(), 56.56046821463599);
    }

    #[test]
    fn qiblah_direction_from_anchorage_in_north_america() {
        let dc = Coordinates::from_degrees(61.2181, -149.9003).unwrap();
        let qiblah = Qiblah::new(dc);

        assert_eq!(qiblah.degrees(), 350.8830761159853);
    }

    #[test]
    fn qiblah_directioon_from_sydney_australia() {
        let sydney = Coordinates::from_degrees(-33.8688, 151.2093).unwrap();
        let qiblah = Qiblah::new(sydney);

        assert_eq!(qiblah.degrees(), 277.4996044487399);
    }

    #[test]
    fn qiblah_directioon_from_auckland_new_zealand() {
        let auckland = Coordinates::from_degrees(-36.8485, 174.7633).unwrap();
        let qiblah = Qiblah::new(auckland);

        assert_eq!(qiblah.degrees(), 261.19732640365845);
    }

    #[test]
    fn qiblah_direction_from_london_united_kingdom() {
        let london = Coordinates::from_degrees(51.5074, -0.1278).unwrap();
        let qiblah = Qiblah::new(london);

        assert!((qiblah.degrees() - 118.9872189).abs() < 1e-7);
    }

    #[test]
    fn qiblah_direction_from_paris_france() {
        let paris = Coordinates::from_degrees(48.8566, 2.3522).unwrap();
        let qiblah = Qiblah::new(paris);

        assert_eq!(qiblah.degrees(), 119.16313542183347);
    }

    #[test]
    fn qiblah_direction_from_oslo_norway() {
        let oslo = Coordinates::from_degrees(59.9139, 10.7522).unwrap();
        let qiblah = Qiblah::new(oslo);

        assert_eq!(qiblah.degrees(), 139.02785605537514);
    }

    #[test]
    fn qiblah_direction_from_islamabad_pakistan() {
        let islamabad = Coordinates::from_degrees(33.7294, 73.0931).unwrap();
        let qiblah = Qiblah::new(islamabad);

        assert_eq!(qiblah.degrees(), 255.8816156785436);
    }

    #[test]
    fn qiblah_direction_from_tokyo_japan() {
        let tokyo = Coordinates::from_degrees(35.6895, 139.6917).unwrap();
        let qiblah = Qiblah::new(tokyo);

        assert_eq!(qiblah.degrees(), 293.02072441441163);
    }

    #[test]
    fn qiblah_direction_from_jakarta_indonesia() {
        let jakarta = Coordinates::from_degrees(-6.18233995, 106.84287154).unwrap();
        let qiblah = Qiblah::new(jakarta);

        assert!((qiblah.degrees() - 295.1442983825265).abs() < 1e-7);
    }

    #[test]
    fn qiblah_direction_display() {
        let nyc = Coordinates::from_degrees(40.7128, -74.0059).unwrap();
        let qiblah = Qiblah::new(nyc);
        let actual_value = qiblah.to_string();

        assert!(actual_value.contains("58.4817635"));
    }
}
