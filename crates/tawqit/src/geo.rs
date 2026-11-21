//! Where: latitude, longitude, a place.

use core::fmt;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::astronomy::unit::{Angle, Normalize};

/// Degrees north of the equator (south negative): a number in [-90, 90].
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "f64", into = "f64"))]
#[derive(PartialEq, PartialOrd, Debug, Copy, Clone)]
pub struct Latitude(f64);

/// Degrees east of Greenwich (west negative), in [-180, 180].
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "f64", into = "f64"))]
#[derive(PartialEq, PartialOrd, Debug, Copy, Clone)]
pub struct Longitude(f64);

/// A number that isn't a latitude: NaN, infinite, or past a pole.
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct LatitudeError(pub f64);

/// A number that isn't a longitude: NaN or infinite.
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct LongitudeError(pub f64);

/// Why two numbers aren't a place.
#[derive(PartialEq, Debug, Copy, Clone)]
pub enum CoordinatesError {
    Latitude(LatitudeError),
    Longitude(LongitudeError),
}

impl Latitude {
    /// `degrees`, if in [-90, 90].
    pub fn new(degrees: f64) -> Result<Latitude, LatitudeError> {
        match (-90.0..=90.0).contains(&degrees) {
            true => Ok(Latitude(degrees)),
            false => Err(LatitudeError(degrees)),
        }
    }

    /// A latitude written in the code, checked when it compiles (only call
    /// it in a `const` context).
    pub(crate) const fn constant(degrees: f64) -> Latitude {
        assert!(degrees >= -90.0 && degrees <= 90.0);
        Latitude(degrees)
    }

    pub fn degrees(self) -> f64 {
        self.0
    }

    /// The same distance from the equator, `steps` half degrees closer to
    /// it, stopping there (AqrabBalad's search).
    pub(crate) fn toward_equator(self, steps: u32) -> Latitude {
        let distance = (self.0.abs() - f64::from(steps) * 0.5).max(0.0);
        Latitude(distance * self.0.signum())
    }

    /// AqrabBalad's half-degree steps to the equator.
    pub(crate) fn steps_to_equator(self) -> u32 {
        (self.0.abs() * 2.0).ceil() as u32
    }
}

impl Longitude {
    /// `degrees`, if finite, wrapped into [-180, 180].
    pub fn new(degrees: f64) -> Result<Longitude, LongitudeError> {
        match degrees {
            d if (-180.0..=180.0).contains(&d) => Ok(Longitude(d)),
            d if d.is_finite() => Ok(Longitude((d + 180.0).normalized_to_scale(360.0) - 180.0)),
            d => Err(LongitudeError(d)),
        }
    }

    pub(crate) const fn constant(degrees: f64) -> Longitude {
        assert!(degrees >= -180.0 && degrees <= 180.0);
        Longitude(degrees)
    }

    pub fn degrees(self) -> f64 {
        self.0
    }
}

/// A place on Earth.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct Coordinates {
    pub latitude: Latitude,
    pub longitude: Longitude,
}

impl Coordinates {
    /// The Kaaba, in Makkah.
    pub const KAABA: Coordinates = Coordinates::new(
        Latitude::constant(21.4225241),
        Longitude::constant(39.8261818),
    );

    pub const fn new(latitude: Latitude, longitude: Longitude) -> Coordinates {
        Coordinates {
            latitude,
            longitude,
        }
    }

    /// The place at `latitude` and `longitude` degrees, if they are.
    pub fn from_degrees(latitude: f64, longitude: f64) -> Result<Coordinates, CoordinatesError> {
        Ok(Coordinates::new(
            Latitude::new(latitude).map_err(CoordinatesError::Latitude)?,
            Longitude::new(longitude).map_err(CoordinatesError::Longitude)?,
        ))
    }

    pub(crate) fn latitude_angle(&self) -> Angle {
        Angle::new(self.latitude.0)
    }

    pub(crate) fn longitude_angle(&self) -> Angle {
        Angle::new(self.longitude.0)
    }
}

impl TryFrom<f64> for Latitude {
    type Error = LatitudeError;

    fn try_from(degrees: f64) -> Result<Self, LatitudeError> {
        Latitude::new(degrees)
    }
}

impl TryFrom<f64> for Longitude {
    type Error = LongitudeError;

    fn try_from(degrees: f64) -> Result<Self, LongitudeError> {
        Longitude::new(degrees)
    }
}

impl From<Latitude> for f64 {
    fn from(latitude: Latitude) -> f64 {
        latitude.0
    }
}

impl From<Longitude> for f64 {
    fn from(longitude: Longitude) -> f64 {
        longitude.0
    }
}

impl fmt::Display for LatitudeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "latitude {} is not a number of degrees in [-90, 90]",
            self.0
        )
    }
}

impl fmt::Display for LongitudeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "longitude {} is not a finite number of degrees", self.0)
    }
}

impl fmt::Display for CoordinatesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoordinatesError::Latitude(e) => e.fmt(f),
            CoordinatesError::Longitude(e) => e.fmt(f),
        }
    }
}

impl core::error::Error for LatitudeError {}
impl core::error::Error for LongitudeError {}
impl core::error::Error for CoordinatesError {}

#[cfg(kani)]
impl kani::Arbitrary for Latitude {
    fn any() -> Self {
        let degrees: f64 = kani::any();
        kani::assume((-90.0..=90.0).contains(&degrees));
        Latitude(degrees)
    }
}

#[cfg(kani)]
impl kani::Arbitrary for Longitude {
    fn any() -> Self {
        let degrees: f64 = kani::any();
        kani::assume((-180.0..=180.0).contains(&degrees));
        Longitude(degrees)
    }
}

#[cfg(kani)]
impl kani::Arbitrary for Coordinates {
    fn any() -> Self {
        Coordinates::new(kani::any(), kani::any())
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any f64 is a latitude exactly when it is in [-90, 90].
    #[kani::proof]
    fn latitude_new_is_total() {
        let degrees: f64 = kani::any();
        match Latitude::new(degrees) {
            Ok(latitude) => {
                assert!(latitude.degrees() == degrees);
            }
            Err(LatitudeError(rejected)) => {
                assert!(rejected.to_bits() == degrees.to_bits());
                assert!(!(-90.0..=90.0).contains(&degrees));
            }
        }
    }

    /// Any f64 is a longitude exactly when finite, then in [-180, 180].
    #[kani::proof]
    fn longitude_new_is_total() {
        let degrees: f64 = kani::any();
        match Longitude::new(degrees) {
            Ok(longitude) => {
                assert!(degrees.is_finite());
                assert!((-180.0..=180.0).contains(&longitude.degrees()));
            }
            Err(LongitudeError(rejected)) => {
                assert!(rejected.to_bits() == degrees.to_bits());
                assert!(!degrees.is_finite());
            }
        }
    }

    /// AqrabBalad's latitudes, any number of steps up to the equator from
    /// any latitude: latitudes, no farther from the equator, on its side;
    /// and at most 180 steps.
    #[kani::proof]
    fn latitudes_toward_the_equator_are_latitudes() {
        let latitude: Latitude = kani::any();
        let steps = latitude.steps_to_equator();
        assert!(steps <= 180);
        let taken: u32 = kani::any();
        kani::assume(taken <= steps);
        let closer = latitude.toward_equator(taken);
        assert!(closer.0.abs() <= latitude.0.abs());
        assert!(closer.0 * latitude.0 >= 0.0);
        assert!(Latitude::new(closer.0).is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latitude_must_be_a_number_of_degrees_within_the_poles() {
        // salah 0.7 took any f64, and NaN or 91° made nonsense times.
        for degrees in [f64::NAN, f64::INFINITY, -f64::INFINITY, 90.5, -91.0] {
            assert!(Latitude::new(degrees).is_err());
        }
        assert!(Latitude::new(90.0).is_ok());
        assert!(Latitude::new(-90.0).is_ok());
    }

    #[test]
    fn longitude_must_be_finite_and_wraps() {
        for degrees in [f64::NAN, f64::INFINITY, -f64::INFINITY] {
            assert!(Longitude::new(degrees).is_err());
        }
        assert_eq!(Longitude::new(190.0).unwrap().degrees(), -170.0);
        assert_eq!(Longitude::new(-540.0).unwrap().degrees(), -180.0);
        assert_eq!(Longitude::new(180.0).unwrap().degrees(), 180.0);
    }

    #[test]
    fn coordinates_from_degrees_say_which_is_wrong() {
        assert_eq!(
            Coordinates::from_degrees(95.0, 0.0),
            Err(CoordinatesError::Latitude(LatitudeError(95.0)))
        );
        assert!(matches!(
            Coordinates::from_degrees(0.0, f64::NAN),
            Err(CoordinatesError::Longitude(_))
        ));
    }

    #[cfg(feature = "serde")]
    #[test]
    fn deserializing_checks_the_numbers() {
        let ok: Coordinates =
            serde_json::from_str(r#"{"latitude":21.4,"longitude":39.8}"#).unwrap();
        assert_eq!(ok, Coordinates::from_degrees(21.4, 39.8).unwrap());
        assert!(serde_json::from_str::<Coordinates>(r#"{"latitude":95,"longitude":0}"#).is_err());
    }
}
