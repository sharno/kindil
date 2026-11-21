// tawqit: began from salah, by Farhan Ahmed (see LICENSE).

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;
use core::f64::consts::PI;
use core::ops::{Add, Mul, Sub};

pub trait Normalize {
    fn normalized_to_scale(&self, max: f64) -> f64;
}

impl Normalize for f64 {
    /// In [0, max) for a positive, finite max; NaN for a NaN or infinite
    /// value. Computed as Adhan does (value - max * floor(value / max)), whose
    /// rounding can land just outside the range: a tiny negative value
    /// lands on max itself, a negative subnormal's quotient underflows and
    /// leaves it below 0, and a value too large for its fraction to be
    /// represented lands anywhere; those are brought back in.
    fn normalized_to_scale(&self, max: f64) -> f64 {
        let normalized = self - (max * (self / max).floor());
        match normalized {
            n if (0.0..max).contains(&n) => n,
            n if n.is_nan() => n,
            n if (0.0..max).contains(&(n + max)) => n + max,
            _ => 0.0,
        }
    }
}

#[derive(PartialEq, Debug, Copy, Clone)]
pub struct Angle {
    pub degrees: f64,
}

impl Angle {
    pub fn new(value: f64) -> Self {
        Angle { degrees: value }
    }

    pub fn from_radians(value: f64) -> Self {
        Angle {
            degrees: (value * 180.0) / PI,
        }
    }

    pub fn radians(&self) -> f64 {
        (self.degrees * PI) / 180.0
    }

    pub fn unwound(&self) -> Angle {
        Angle {
            degrees: self.degrees.normalized_to_scale(360.0),
        }
    }

    /// The same direction in [-180, 180].
    pub fn quadrant_shifted(&self) -> Angle {
        if (-180.0..=180.0).contains(&self.degrees) {
            *self
        } else {
            Angle {
                degrees: self.degrees - (360.0 * (self.degrees / 360.0).round()),
            }
        }
    }
}

impl Add for Angle {
    type Output = Angle;

    fn add(self, rhs: Angle) -> Angle {
        Angle {
            degrees: self.degrees + rhs.degrees,
        }
    }
}

impl Sub for Angle {
    type Output = Angle;

    fn sub(self, rhs: Angle) -> Angle {
        Angle {
            degrees: self.degrees - rhs.degrees,
        }
    }
}

impl Mul for Angle {
    type Output = Angle;

    fn mul(self, rhs: Angle) -> Angle {
        Angle {
            degrees: self.degrees * rhs.degrees,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn angle_conversion_from_radians() {
        assert_eq!(Angle::from_radians(PI).degrees, 180.0);
        assert_eq!(Angle::from_radians(PI / 2.0).degrees, 90.0);
    }

    #[test]
    fn angle_conversion_degrees_to_radians() {
        assert_eq!(Angle::new(180.0).radians(), PI);
        assert_eq!(Angle::new(90.0).radians(), PI / 2.0);
    }

    #[test]
    fn normalize_value() {
        assert_eq!((-1.0_f64).normalized_to_scale(24.0), 23.0);
        assert_eq!(1.0_f64.normalized_to_scale(24.0), 1.0);
        assert_eq!(49.0_f64.normalized_to_scale(24.0), 1.0);

        assert_eq!(361.0_f64.normalized_to_scale(360.0), 1.0);
        assert_eq!(360.0_f64.normalized_to_scale(360.0), 0.0);
        assert_eq!(259.0_f64.normalized_to_scale(360.0), 259.0);
        assert_eq!(2592.0_f64.normalized_to_scale(360.0), 72.0);
    }

    #[test]
    fn angle_unwound() {
        assert_eq!(Angle::new(-45.0).unwound().degrees, 315.0);
        assert_eq!(Angle::new(361.0).unwound().degrees, 1.0);
        assert_eq!(Angle::new(360.0).unwound().degrees, 0.0);
        assert_eq!(Angle::new(259.0).unwound().degrees, 259.0);
        assert_eq!(Angle::new(2592.0).unwound().degrees, 72.0);
    }

    #[test]
    fn unwound_stays_below_360() {
        // salah 0.7 unwound -1e-17° to 360°, a Qiblah bearing out of range.
        assert_eq!(Angle::new(-1e-17).unwound().degrees, 0.0);
        assert_eq!((-1e-17_f64).normalized_to_scale(1.0), 0.0);
        // A negative subnormal's quotient underflows to -0.0: salah 0.7 (and
        // the first fix) left the value itself, below 0. Found by Kani.
        assert_eq!((-5e-324_f64).normalized_to_scale(360.0), 0.0);
    }

    #[test]
    fn closest_angle() {
        assert_eq!(Angle::new(360.0).quadrant_shifted().degrees, 0.0);
        assert_eq!(Angle::new(361.0).quadrant_shifted().degrees, 1.0);
        assert_eq!(Angle::new(1.0).quadrant_shifted().degrees, 1.0);
        assert_eq!(Angle::new(-1.0).quadrant_shifted().degrees, -1.0);
        assert_eq!(Angle::new(-181.0).quadrant_shifted().degrees, 179.0);
        assert_eq!(Angle::new(180.0).quadrant_shifted().degrees, 180.0);
        assert_eq!(Angle::new(359.0).quadrant_shifted().degrees, -1.0);
        assert_eq!(Angle::new(-359.0).quadrant_shifted().degrees, 1.0);
        assert_eq!(Angle::new(1261.0).quadrant_shifted().degrees, -179.0);
    }

    #[test]
    fn adding_angles() {
        let angle_a = Angle::new(45.0);
        let angle_b = Angle::new(45.0);

        assert_eq!((angle_a + angle_b).degrees, 90.0);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any finite value normalizes into [0, max), for each scale used.
    #[kani::proof]
    fn normalized_to_scale_is_in_range() {
        let value: f64 = kani::any();
        kani::assume(value.is_finite());
        for max in [1.0, 24.0, 360.0] {
            let normalized = value.normalized_to_scale(max);
            assert!((0.0..max).contains(&normalized));
        }
    }
}
