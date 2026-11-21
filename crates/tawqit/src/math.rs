// Salah
//
// See LICENSE for more details.

//! f64's math without std: the same names as std's inherent methods, from
//! libm. With std the inherent methods are used, and this isn't built.

// Newer toolchains' core has these methods too, inherent, so they win and
// the trait goes unused; older ones (down to the crate's rust-version) need
// it.
#[allow(dead_code)]
pub(crate) trait Float: Sized {
    fn sin(self) -> f64;
    fn cos(self) -> f64;
    fn tan(self) -> f64;
    fn asin(self) -> f64;
    fn acos(self) -> f64;
    fn atan(self) -> f64;
    fn atan2(self, other: f64) -> f64;
    fn floor(self) -> f64;
    fn ceil(self) -> f64;
    fn round(self) -> f64;
    fn abs(self) -> f64;
    fn powf(self, n: f64) -> f64;
    fn rem_euclid(self, rhs: f64) -> f64;
}

impl Float for f64 {
    fn sin(self) -> f64 {
        libm::sin(self)
    }
    fn cos(self) -> f64 {
        libm::cos(self)
    }
    fn tan(self) -> f64 {
        libm::tan(self)
    }
    fn asin(self) -> f64 {
        libm::asin(self)
    }
    fn acos(self) -> f64 {
        libm::acos(self)
    }
    fn atan(self) -> f64 {
        libm::atan(self)
    }
    fn atan2(self, other: f64) -> f64 {
        libm::atan2(self, other)
    }
    fn floor(self) -> f64 {
        libm::floor(self)
    }
    fn ceil(self) -> f64 {
        libm::ceil(self)
    }
    fn round(self) -> f64 {
        libm::round(self)
    }
    fn abs(self) -> f64 {
        libm::fabs(self)
    }
    fn powf(self, n: f64) -> f64 {
        libm::pow(self, n)
    }
    fn rem_euclid(self, rhs: f64) -> f64 {
        let r = self % rhs;
        if r < 0.0 { r + Float::abs(rhs) } else { r }
    }
}

#[cfg(test)]
mod tests {
    use super::Float;

    /// Tests link std, whose inherent methods shadow these; called through
    /// the trait, they are libm's, and agree with std's.
    #[test]
    fn libm_agrees_with_std() {
        let close = |a: f64, b: f64| {
            (a - b).abs() <= 1e-12 * b.abs().max(1.0) || (a.is_nan() && b.is_nan())
        };
        let values = (-2000..=2000).map(|i| f64::from(i) * 0.37);
        for x in values {
            assert!(close(Float::sin(x), x.sin()), "sin {x}");
            assert!(close(Float::cos(x), x.cos()), "cos {x}");
            assert!(close(Float::tan(x), x.tan()), "tan {x}");
            assert!(
                close(Float::asin(x / 740.0), (x / 740.0).asin()),
                "asin {x}"
            );
            assert!(
                close(Float::acos(x / 740.0), (x / 740.0).acos()),
                "acos {x}"
            );
            assert!(close(Float::atan(x), x.atan()), "atan {x}");
            assert!(
                close(Float::atan2(x, 3.0 - x), x.atan2(3.0 - x)),
                "atan2 {x}"
            );
            assert_eq!(Float::floor(x), x.floor());
            assert_eq!(Float::ceil(x), x.ceil());
            assert_eq!(Float::round(x), x.round());
            assert_eq!(Float::abs(x), x.abs());
            assert!(
                close(Float::powf(x.abs(), 2.0), x.abs().powf(2.0)),
                "powf {x}"
            );
            assert!(
                close(Float::rem_euclid(x, 360.0), x.rem_euclid(360.0)),
                "rem_euclid {x}"
            );
            assert!(
                close(Float::rem_euclid(x, 24.0), x.rem_euclid(24.0)),
                "rem_euclid {x}"
            );
        }
    }
}
