// Salah
//
// See LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//
use core::fmt;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use super::adjustments::TimeAdjustment;
use super::high_altitude_rule::HighLatitudeRule;
use super::madhab::Madhab;
use super::method::Method;
use super::polar_circle_resolution::PolarCircleResolution;
use super::prayer::Prayer;
use super::rounding::Rounding;
use super::shafaq::Shafaq;

/// Degrees of the sun's center below the horizon, for a twilight: a finite
/// number in [0, 30]. Every method's angles are in 4.5 to 20; past 30 the
/// twilight angle rule's share of the night (angle / 60) would pass its
/// middle.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "f64", into = "f64"))]
#[derive(PartialEq, PartialOrd, Debug, Copy, Clone)]
pub struct DepressionAngle(f64);

/// A number that isn't a [`DepressionAngle`].
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct AngleError(pub f64);

impl DepressionAngle {
    /// The largest angle taken.
    pub const MAX: f64 = 30.0;

    /// `degrees` below the horizon, if finite and in [0, 30].
    pub fn new(degrees: f64) -> Result<Self, AngleError> {
        if (0.0..=Self::MAX).contains(&degrees) {
            Ok(DepressionAngle(degrees))
        } else {
            Err(AngleError(degrees))
        }
    }

    /// A method's angle, checked when the constant is built (only call it
    /// in a `const` context).
    pub(crate) const fn constant(degrees: f64) -> Self {
        assert!(degrees >= 0.0 && degrees <= Self::MAX);
        DepressionAngle(degrees)
    }

    pub fn degrees(self) -> f64 {
        self.0
    }
}

/// Any angle `new` takes.
#[cfg(kani)]
impl kani::Arbitrary for DepressionAngle {
    fn any() -> Self {
        let degrees: f64 = kani::any();
        kani::assume((0.0..=Self::MAX).contains(&degrees));
        DepressionAngle(degrees)
    }
}

impl TryFrom<f64> for DepressionAngle {
    type Error = AngleError;

    fn try_from(degrees: f64) -> Result<Self, AngleError> {
        DepressionAngle::new(degrees)
    }
}

impl From<DepressionAngle> for f64 {
    fn from(angle: DepressionAngle) -> f64 {
        angle.0
    }
}

impl fmt::Display for AngleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is not a number of degrees below the horizon in [0, {}]",
            self.0,
            DepressionAngle::MAX
        )
    }
}

impl core::error::Error for AngleError {}

/// When Isha begins.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum Isha {
    /// When the sun is this far below the horizon after sunset.
    Angle(DepressionAngle),
    /// This many minutes after sunset.
    Interval(u8),
}

/// Settings that are used for determining the
/// the correct prayer time.
///
/// It is recommended to use [Configuration](struct.Configuration.html) to build
/// the parameters that are need.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub struct Parameters {
    pub method: Method,
    pub fajr_angle: DepressionAngle,
    /// Maghrib when the sun is this far below the horizon, rather than at
    /// sunset (Tehran's method).
    pub maghrib_angle: Option<DepressionAngle>,
    pub isha: Isha,
    pub madhab: Madhab,
    pub high_latitude_rule: HighLatitudeRule,
    pub polar_circle_resolution: PolarCircleResolution,
    pub adjustments: TimeAdjustment,
    pub method_adjustments: TimeAdjustment,
    pub rounding: Rounding,
    pub shafaq: Shafaq,
}

impl Parameters {
    pub fn new(fajr_angle: DepressionAngle, isha: Isha) -> Parameters {
        Configuration::new(fajr_angle, isha).done()
    }

    /// The share of the night a twilight at `angle` is held to under the
    /// high-latitude rule: Fajr no earlier than this share of the night
    /// before sunrise, Isha no later than this share after sunset.
    pub fn night_portion(&self, angle: DepressionAngle) -> f64 {
        match self.high_latitude_rule {
            HighLatitudeRule::MiddleOfTheNight => 1.0 / 2.0,
            HighLatitudeRule::SeventhOfTheNight => 1.0 / 7.0,
            HighLatitudeRule::TwilightAngle => angle.degrees() / 60.0,
        }
    }

    /// The minutes added to `prayer`'s time: the user's and the method's.
    pub fn time_adjustments(&self, prayer: Prayer) -> i32 {
        let pick = |a: &TimeAdjustment| match prayer {
            Prayer::Fajr | Prayer::FajrTomorrow => a.fajr,
            Prayer::Sunrise => a.sunrise,
            Prayer::Dhuhr => a.dhuhr,
            Prayer::Asr => a.asr,
            Prayer::Maghrib => a.maghrib,
            Prayer::Isha => a.isha,
            Prayer::Qiyam => 0,
        };
        i32::from(pick(&self.adjustments)) + i32::from(pick(&self.method_adjustments))
    }
}

/// A builder for the the [Parameters](struct.Parameters.html).
///
/// It is recommended that this is used for setting
/// all parameters that are needed.
pub struct Configuration {
    parameters: Parameters,
}

impl Configuration {
    pub fn new(fajr_angle: DepressionAngle, isha: Isha) -> Configuration {
        Configuration {
            parameters: Parameters {
                method: Method::Other,
                fajr_angle,
                maghrib_angle: None,
                isha,
                madhab: Madhab::Shafi,
                high_latitude_rule: HighLatitudeRule::MiddleOfTheNight,
                polar_circle_resolution: PolarCircleResolution::Unresolved,
                adjustments: TimeAdjustment::default(),
                method_adjustments: TimeAdjustment::default(),
                rounding: Rounding::Nearest,
                shafaq: Shafaq::General,
            },
        }
    }

    pub fn with(method: Method, madhab: Madhab) -> Parameters {
        Parameters {
            madhab,
            ..method.parameters()
        }
    }

    pub fn method(&mut self, method: Method) -> &mut Configuration {
        self.parameters.method = method;
        self
    }

    pub fn adjustments(&mut self, adjustments: TimeAdjustment) -> &mut Configuration {
        self.parameters.adjustments = adjustments;
        self
    }

    pub fn method_adjustments(&mut self, method_adjustments: TimeAdjustment) -> &mut Configuration {
        self.parameters.method_adjustments = method_adjustments;
        self
    }

    pub fn high_latitude_rule(
        &mut self,
        high_latitude_rule: HighLatitudeRule,
    ) -> &mut Configuration {
        self.parameters.high_latitude_rule = high_latitude_rule;
        self
    }

    pub fn polar_circle_resolution(
        &mut self,
        resolution: PolarCircleResolution,
    ) -> &mut Configuration {
        self.parameters.polar_circle_resolution = resolution;
        self
    }

    pub fn madhab(&mut self, madhab: Madhab) -> &mut Configuration {
        self.parameters.madhab = madhab;
        self
    }

    /// Isha this many minutes after sunset, rather than at an angle.
    pub fn isha_interval(&mut self, minutes: u8) -> &mut Configuration {
        self.parameters.isha = Isha::Interval(minutes);
        self
    }

    pub fn maghrib_angle(&mut self, angle: DepressionAngle) -> &mut Configuration {
        self.parameters.maghrib_angle = Some(angle);
        self
    }

    pub fn rounding(&mut self, value: Rounding) -> &mut Configuration {
        self.parameters.rounding = value;
        self
    }

    pub fn shafaq(&mut self, value: Shafaq) -> &mut Configuration {
        self.parameters.shafaq = value;
        self
    }

    pub fn done(&self) -> Parameters {
        self.parameters
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any f64 is an angle exactly when it is in [0, 30], and the night
    /// portion of any angle under any rule is in (0, 1/2].
    #[kani::proof]
    fn angles_and_night_portions() {
        let degrees: f64 = kani::any();
        match DepressionAngle::new(degrees) {
            Ok(angle) => {
                assert!(angle.degrees() == degrees);
                let parameters: Parameters = kani::any();
                let portion = parameters.night_portion(angle);
                assert!((0.0..=0.5).contains(&portion));
            }
            Err(AngleError(rejected)) => {
                assert!(rejected.to_bits() == degrees.to_bits());
                assert!(!(0.0..=DepressionAngle::MAX).contains(&degrees));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn angle(degrees: f64) -> DepressionAngle {
        DepressionAngle::new(degrees).unwrap()
    }

    #[test]
    fn angles_are_finite_degrees_from_0_to_30() {
        // salah 0.7 took any f64: NaN fell back to the safe time silently,
        // and a huge angle under the twilight angle rule overflowed.
        for bad in [f64::NAN, f64::INFINITY, -f64::INFINITY, -0.5, 30.5, 1e300] {
            assert_eq!(
                DepressionAngle::new(bad).map_err(|e| e.0.to_bits()),
                Err(bad.to_bits())
            );
        }
        assert_eq!(angle(0.0).degrees(), 0.0);
        assert_eq!(angle(30.0).degrees(), 30.0);
    }

    #[test]
    fn calculate_parameters_with_fajr_and_isha_angles() {
        let params = Parameters::new(angle(18.0), Isha::Angle(angle(18.0)));

        assert_eq!(params.fajr_angle, angle(18.0));
        assert_eq!(params.isha, Isha::Angle(angle(18.0)));
    }

    #[test]
    fn calculated_night_portions_middle_of_the_night() {
        let params = Parameters::new(angle(18.0), Isha::Angle(angle(18.0)));

        assert_eq!(params.night_portion(angle(18.0)), 1.0 / 2.0);
    }

    #[test]
    fn calculated_night_portions_seventh_of_the_night() {
        let params = Configuration::new(angle(18.0), Isha::Angle(angle(18.0)))
            .high_latitude_rule(HighLatitudeRule::SeventhOfTheNight)
            .done();

        assert_eq!(params.night_portion(angle(18.0)), 1.0 / 7.0);
    }

    #[test]
    fn calculated_night_portions_twilight_angle() {
        let params = Configuration::new(angle(10.0), Isha::Angle(angle(15.0)))
            .high_latitude_rule(HighLatitudeRule::TwilightAngle)
            .done();

        assert_eq!(params.night_portion(angle(10.0)), 10.0 / 60.0);
        assert_eq!(params.night_portion(angle(15.0)), 15.0 / 60.0);
    }

    #[test]
    fn parameters_using_method_and_madhab() {
        let params = Configuration::with(Method::NorthAmerica, Madhab::Hanafi);

        assert_eq!(params.method, Method::NorthAmerica);
        assert_eq!(params.fajr_angle, angle(15.0));
        assert_eq!(params.isha, Isha::Angle(angle(15.0)));
        assert_eq!(params.madhab, Madhab::Hanafi);
    }

    #[cfg(feature = "serde")]
    #[test]
    fn deserializing_checks_angles() {
        assert_eq!(
            serde_json::from_str::<DepressionAngle>("18.5").unwrap(),
            angle(18.5)
        );
        assert!(serde_json::from_str::<DepressionAngle>("95").is_err());
    }
}
