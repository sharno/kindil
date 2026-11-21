// Salah
//
// See LICENSE for more details.

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;
use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::astronomy::unit::{Angle, Normalize};

/// A place on Earth: a latitude in [-90°, 90°] and a longitude in
/// [-180°, 180°], both finite. Make one with [`Coordinates::new`].
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "RawCoordinates"))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct Coordinates {
    pub(crate) latitude: f64,
    pub(crate) longitude: f64,
}

/// Why numbers aren't coordinates.
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum CoordinatesError {
    /// Not a number in [-90, 90].
    InvalidLatitude(f64),
    /// Not a finite number.
    InvalidLongitude(f64),
}

impl Coordinates {
    /// The place at `latitude` and `longitude`, in degrees (north and east
    /// positive). Any finite longitude is taken, wrapped into [-180°, 180°].
    pub fn new(latitude: f64, longitude: f64) -> Result<Self, CoordinatesError> {
        if !(-90.0..=90.0).contains(&latitude) {
            return Err(CoordinatesError::InvalidLatitude(latitude));
        }
        if !longitude.is_finite() {
            return Err(CoordinatesError::InvalidLongitude(longitude));
        }
        let longitude = match longitude {
            l if (-180.0..=180.0).contains(&l) => l,
            l => (l + 180.0).normalized_to_scale(360.0) - 180.0,
        };
        Ok(Coordinates {
            latitude,
            longitude,
        })
    }

    /// Degrees north of the equator (south negative).
    pub fn latitude(&self) -> f64 {
        self.latitude
    }

    /// Degrees east of Greenwich (west negative).
    pub fn longitude(&self) -> f64 {
        self.longitude
    }

    pub(crate) fn latitude_angle(&self) -> Angle {
        Angle::new(self.latitude)
    }

    pub(crate) fn longitude_angle(&self) -> Angle {
        Angle::new(self.longitude)
    }
}

/// Any coordinates `new` makes.
#[cfg(kani)]
impl kani::Arbitrary for Coordinates {
    fn any() -> Self {
        let (latitude, longitude): (f64, f64) = (kani::any(), kani::any());
        kani::assume((-90.0..=90.0).contains(&latitude));
        kani::assume((-180.0..=180.0).contains(&longitude));
        Coordinates {
            latitude,
            longitude,
        }
    }
}

impl fmt::Display for CoordinatesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoordinatesError::InvalidLatitude(l) => {
                write!(f, "latitude {l} is not a number of degrees in [-90, 90]")
            }
            CoordinatesError::InvalidLongitude(l) => {
                write!(f, "longitude {l} is not a finite number of degrees")
            }
        }
    }
}

impl core::error::Error for CoordinatesError {}

/// Coordinates as they come in, before they're checked.
#[cfg(feature = "serde")]
#[derive(Deserialize)]
struct RawCoordinates {
    latitude: f64,
    longitude: f64,
}

#[cfg(feature = "serde")]
impl TryFrom<RawCoordinates> for Coordinates {
    type Error = CoordinatesError;

    fn try_from(raw: RawCoordinates) -> Result<Self, Self::Error> {
        Coordinates::new(raw.latitude, raw.longitude)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any two f64 (NaN, infinities, any size) are coordinates exactly when
    /// the latitude is in [-90, 90] and the longitude finite, and then the
    /// longitude is in [-180, 180].
    #[kani::proof]
    fn new_is_total() {
        let (latitude, longitude): (f64, f64) = (kani::any(), kani::any());
        match Coordinates::new(latitude, longitude) {
            Ok(coordinates) => {
                assert!(coordinates.latitude() == latitude);
                assert!((-180.0..=180.0).contains(&coordinates.longitude()));
                assert!(longitude.is_finite());
            }
            Err(CoordinatesError::InvalidLatitude(_)) => {
                assert!(!(-90.0..=90.0).contains(&latitude));
            }
            Err(CoordinatesError::InvalidLongitude(_)) => {
                assert!(!longitude.is_finite());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latitude_must_be_a_number_of_degrees_within_the_poles() {
        // salah 0.7 took any f64, and NaN or 91° made nonsense times.
        for latitude in [f64::NAN, f64::INFINITY, -f64::INFINITY, 90.5, -91.0] {
            assert!(matches!(
                Coordinates::new(latitude, 0.0),
                Err(CoordinatesError::InvalidLatitude(_))
            ));
        }
        assert!(Coordinates::new(90.0, 0.0).is_ok());
        assert!(Coordinates::new(-90.0, 0.0).is_ok());
    }

    #[test]
    fn longitude_must_be_finite_and_wraps() {
        for longitude in [f64::NAN, f64::INFINITY, -f64::INFINITY] {
            assert!(matches!(
                Coordinates::new(0.0, longitude),
                Err(CoordinatesError::InvalidLongitude(_))
            ));
        }
        assert_eq!(Coordinates::new(0.0, 190.0).unwrap().longitude(), -170.0);
        assert_eq!(Coordinates::new(0.0, -540.0).unwrap().longitude(), -180.0);
        assert_eq!(Coordinates::new(0.0, 180.0).unwrap().longitude(), 180.0);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn deserializing_checks_the_numbers() {
        let ok: Coordinates =
            serde_json::from_str(r#"{"latitude":21.4,"longitude":39.8}"#).unwrap();
        assert_eq!(ok, Coordinates::new(21.4, 39.8).unwrap());
        assert!(serde_json::from_str::<Coordinates>(r#"{"latitude":95,"longitude":0}"#).is_err());
    }
}
