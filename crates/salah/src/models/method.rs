// Salah
//
// See README.md and LICENSE for more details.
// Copyright (c) 2019-2022 Farhan Ahmed. All rights reserved.
//

use super::adjustments::TimeAdjustment;
use super::parameters::{Configuration, DepressionAngle, Isha, Parameters};
use super::rounding::Rounding;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// Provides preset configuration for a few authorities
/// for calculating prayer times.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(PartialEq, Debug, Copy, Clone)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
pub enum Method {
    /// Muslim World League. Standard Fajr time with an angle of 18°.
    /// Earlier Isha time with an angle of 17°.
    MuslimWorldLeague,

    /// Egyptian General Authority of Survey. Early Fajr time using an angle 19.5°
    /// and a slightly earlier Isha time using an angle of 17.5°.
    Egyptian,

    /// University of Islamic Sciences, Karachi. A generally applicable method that
    /// uses standard Fajr and Isha angles of 18°.
    Karachi,

    /// Umm al-Qura University, Makkah. Uses a fixed interval of 90 minutes
    /// from maghrib to calculate Isha. And a slightly earlier Fajr time with
    /// an angle of 18.5°. Note: you should add a +30 minute custom adjustment
    /// for Isha during Ramadan.
    UmmAlQura,

    /// Used in the UAE. Slightly earlier Fajr time and slightly later Isha
    /// time with angles of 18.2° for Fajr and Isha in addition to 3 minute
    /// offsets for sunrise, Dhuhr, Asr, and Maghrib.
    Dubai,

    /// Method developed by Khalid Shaukat, founder of Moonsighting Committee Worldwide.
    /// Uses standard 18° angles for Fajr and Isha in addition to seasonal adjustment values.
    /// This method automatically applies the 1/7 approximation rule for locations above 55°
    /// latitude. Recommended for North America and the UK.
    MoonsightingCommittee,

    /// Also known as the ISNA method. Can be used for North America,
    /// but the moonsightingCommittee method is preferable. Gives later Fajr times and early.
    /// Isha times with angles of 15°.
    NorthAmerica,

    /// Standard Fajr time with an angle of 18°. Slightly earlier Isha time with an angle of 17.5°.
    Kuwait,

    /// Same Isha interval as `ummAlQura` but with the standard Fajr time using an angle of 18°.
    Qatar,

    /// Used in Singapore, Malaysia, and Indonesia. Early Fajr time with an angle of 20°
    /// and standard Isha time with an angle of 18°.
    Singapore,

    /// Institute of Geophysics, University of Tehran. Early Isha time with an angle of 14°.
    /// Slightly later Fajr time with an angle of 17.7°. Calculates Maghrib based on the sun
    /// reaching an angle of 4.5° below the horizon.
    Tehran,

    /// An approximation of the Diyanet method used in Turkey.
    /// This approximation is less accurate outside the region of Turkey.
    Turkey,

    /// Defaults to angles of 0°, should generally be used for making a custom method
    /// and setting your own values.
    Other,
}

/// A method's angles, offsets and rounding.
struct Preset {
    fajr: DepressionAngle,
    isha: Isha,
    maghrib: Option<DepressionAngle>,
    adjustments: TimeAdjustment,
    rounding: Rounding,
}

const fn angle(degrees: f64) -> Isha {
    Isha::Angle(DepressionAngle::constant(degrees))
}

/// Minutes added to sunrise, Dhuhr, Asr and Maghrib.
const fn offsets(sunrise: i16, dhuhr: i16, asr: i16, maghrib: i16) -> TimeAdjustment {
    TimeAdjustment {
        fajr: 0,
        sunrise,
        dhuhr,
        asr,
        maghrib,
        isha: 0,
    }
}

const fn preset(fajr: f64, isha: Isha, adjustments: TimeAdjustment) -> Preset {
    Preset {
        fajr: DepressionAngle::constant(fajr),
        isha,
        maghrib: None,
        adjustments,
        rounding: Rounding::Nearest,
    }
}

impl Method {
    /// Each arm is a `const` block, so the angles are checked when the
    /// crate compiles.
    fn preset(self) -> Preset {
        match self {
            Method::MuslimWorldLeague => const { preset(18.0, angle(17.0), offsets(0, 1, 0, 0)) },
            Method::Egyptian => const { preset(19.5, angle(17.5), offsets(0, 1, 0, 0)) },
            Method::Karachi => const { preset(18.0, angle(18.0), offsets(0, 1, 0, 0)) },
            Method::UmmAlQura => const { preset(18.5, Isha::Interval(90), offsets(0, 0, 0, 0)) },
            Method::Dubai => const { preset(18.2, angle(18.2), offsets(-3, 3, 3, 3)) },
            Method::MoonsightingCommittee => {
                const { preset(18.0, angle(18.0), offsets(0, 5, 0, 3)) }
            }
            Method::NorthAmerica => const { preset(15.0, angle(15.0), offsets(0, 1, 0, 0)) },
            Method::Kuwait => const { preset(18.0, angle(17.5), offsets(0, 0, 0, 0)) },
            Method::Qatar => const { preset(18.0, Isha::Interval(90), offsets(0, 0, 0, 0)) },
            Method::Singapore => {
                const {
                    Preset {
                        rounding: Rounding::Up,
                        ..preset(20.0, angle(18.0), offsets(0, 1, 0, 0))
                    }
                }
            }
            Method::Tehran => {
                const {
                    Preset {
                        maghrib: Some(DepressionAngle::constant(4.5)),
                        ..preset(17.7, angle(14.0), offsets(0, 0, 0, 0))
                    }
                }
            }
            Method::Turkey => const { preset(18.0, angle(17.0), offsets(-7, 5, 4, 7)) },
            Method::Other => const { preset(0.0, angle(0.0), offsets(0, 0, 0, 0)) },
        }
    }

    pub fn parameters(&self) -> Parameters {
        let preset = self.preset();
        let mut configuration = Configuration::new(preset.fajr, preset.isha);
        configuration
            .method(*self)
            .method_adjustments(preset.adjustments)
            .rounding(preset.rounding);
        if let Some(maghrib) = preset.maghrib {
            configuration.maghrib_angle(maghrib);
        }
        configuration.done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_for_muslim_world_league() {
        let method = Method::MuslimWorldLeague;
        let params = method.parameters();

        assert_eq!(params.method, Method::MuslimWorldLeague);
        assert_eq!(params.fajr_angle.degrees(), 18.0);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(17.0).unwrap())
        );
    }

    #[test]
    fn parameters_for_egyptian() {
        let method = Method::Egyptian;
        let params = method.parameters();

        assert_eq!(params.method, Method::Egyptian);
        assert_eq!(params.fajr_angle.degrees(), 19.5);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(17.5).unwrap())
        );
    }

    #[test]
    fn parameters_for_karachi() {
        let method = Method::Karachi;
        let params = method.parameters();

        assert_eq!(params.method, Method::Karachi);
        assert_eq!(params.fajr_angle.degrees(), 18.0);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(18.0).unwrap())
        );
    }

    #[test]
    fn parameters_for_umm_al_qura() {
        let method = Method::UmmAlQura;
        let params = method.parameters();

        assert_eq!(params.method, Method::UmmAlQura);
        assert_eq!(params.fajr_angle.degrees(), 18.5);
        assert_eq!(params.isha, Isha::Interval(90));
    }

    #[test]
    fn parameters_for_dubai() {
        let method = Method::Dubai;
        let params = method.parameters();

        assert_eq!(params.method, Method::Dubai);
        assert_eq!(
            params.fajr_angle.degrees(),
            18.2,
            "Parameters: {:?}",
            params
        );
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(18.2).unwrap())
        );
    }

    #[test]
    fn parameters_for_moonsighting_committee() {
        let method = Method::MoonsightingCommittee;
        let params = method.parameters();

        assert_eq!(params.method, Method::MoonsightingCommittee);
        assert_eq!(params.fajr_angle.degrees(), 18.0);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(18.0).unwrap())
        );
    }

    #[test]
    fn parameters_for_north_america() {
        let method = Method::NorthAmerica;
        let params = method.parameters();

        assert_eq!(params.method, Method::NorthAmerica);
        assert_eq!(params.fajr_angle.degrees(), 15.0);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(15.0).unwrap())
        );
    }

    #[test]
    fn parameters_for_kuwait() {
        let method = Method::Kuwait;
        let params = method.parameters();

        assert_eq!(params.method, Method::Kuwait);
        assert_eq!(params.fajr_angle.degrees(), 18.0);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(17.5).unwrap())
        );
    }

    #[test]
    fn parameters_for_qatar() {
        let method = Method::Qatar;
        let params = method.parameters();

        assert_eq!(params.method, Method::Qatar);
        assert_eq!(params.fajr_angle.degrees(), 18.0);
        assert_eq!(params.isha, Isha::Interval(90));
    }

    #[test]
    fn parameters_for_singapore() {
        let method = Method::Singapore;
        let params = method.parameters();

        assert_eq!(params.method, Method::Singapore);
        assert_eq!(params.fajr_angle.degrees(), 20.0);
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(18.0).unwrap())
        );
    }

    #[test]
    fn parameters_for_tehran() {
        let params = Method::Tehran.parameters();

        assert_eq!(params.maghrib_angle, DepressionAngle::new(4.5).ok());
        assert_eq!(
            params.isha,
            Isha::Angle(DepressionAngle::new(14.0).unwrap())
        );
    }

    #[test]
    fn parameters_for_singapore_round_up() {
        assert_eq!(Method::Singapore.parameters().rounding, Rounding::Up);
    }

    #[test]
    fn parameters_for_other() {
        let method = Method::Other;
        let params = method.parameters();

        assert_eq!(params.method, Method::Other);
        assert_eq!(params.fajr_angle.degrees(), 0.0);
        assert_eq!(params.isha, Isha::Angle(DepressionAngle::new(0.0).unwrap()));
    }
}
