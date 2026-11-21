//! How: the angles, rules and offsets a calculation method is made of.
//!
//! Every value is a type that can only hold what it means (one fallible
//! constructor each), so any `Parameters` that can be built is valid; the
//! methods' presets are constants, checked when the crate compiles.

use core::fmt;

#[cfg(not(feature = "std"))]
#[allow(unused_imports)]
use crate::math::Float;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::geo::Coordinates;
use crate::verified;

/// Degrees of the sun's center below the horizon, for a twilight: in
/// (0, 30], to a hundredth of a degree (every method's angles are; the
/// rule that takes a share of the night by the angle, angle / 60, then is
/// an exact fraction). Past 30° that share would pass the night's middle.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "f64", into = "f64"))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Copy, Clone, Hash)]
pub struct DepressionAngle(u16);

/// A number that isn't a [`DepressionAngle`].
#[derive(PartialEq, Debug, Copy, Clone)]
pub struct AngleError(pub f64);

impl DepressionAngle {
    /// The largest angle, in degrees.
    pub const MAX_DEGREES: f64 = 30.0;

    /// `degrees` below the horizon, to the nearest hundredth, if that is in
    /// (0, 30].
    pub fn new(degrees: f64) -> Result<DepressionAngle, AngleError> {
        let hundredths = (degrees * 100.0).round();
        match (1.0..=3000.0).contains(&hundredths) {
            true => Ok(DepressionAngle(hundredths as u16)),
            false => Err(AngleError(degrees)),
        }
    }

    /// An angle in hundredths of a degree written in the code, checked when
    /// it compiles (only call it in a `const` context).
    pub(crate) const fn hundredths(hundredths: u16) -> DepressionAngle {
        assert!(hundredths >= 1 && hundredths <= 3000);
        DepressionAngle(hundredths)
    }

    pub fn degrees(self) -> f64 {
        f64::from(self.0) / 100.0
    }

    /// The twilight angle rule's share of the night: angle / 60.
    pub(crate) fn night_share(self) -> verified::Share {
        verified::Share {
            numerator: i64::from(self.0),
            denominator: 6000,
        }
    }
}

/// Isha this many minutes after sunset: 5 to 240 (less would fall within
/// five minutes of Maghrib, which the order guard takes no times for).
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "u8", into = "u8"))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Copy, Clone, Hash)]
pub struct IshaInterval(u8);

/// A number of minutes that isn't an [`IshaInterval`].
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub struct IntervalError(pub u8);

impl IshaInterval {
    pub fn new(minutes: u8) -> Result<IshaInterval, IntervalError> {
        match (5..=240).contains(&minutes) {
            true => Ok(IshaInterval(minutes)),
            false => Err(IntervalError(minutes)),
        }
    }

    pub(crate) const fn constant(minutes: u8) -> IshaInterval {
        assert!(minutes >= 5 && minutes <= 240);
        IshaInterval(minutes)
    }

    pub fn minutes(self) -> u8 {
        self.0
    }
}

/// Minutes added to a time (negative: taken away), at most 12 hours either
/// way.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(try_from = "i16", into = "i16"))]
#[derive(PartialEq, Eq, PartialOrd, Ord, Debug, Copy, Clone, Hash)]
pub struct MinuteOffset(i16);

/// A number of minutes that isn't a [`MinuteOffset`].
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub struct OffsetError(pub i16);

impl MinuteOffset {
    pub const ZERO: MinuteOffset = MinuteOffset(0);
    pub const MAX_MINUTES: i16 = 720;

    pub fn new(minutes: i16) -> Result<MinuteOffset, OffsetError> {
        match (-Self::MAX_MINUTES..=Self::MAX_MINUTES).contains(&minutes) {
            true => Ok(MinuteOffset(minutes)),
            false => Err(OffsetError(minutes)),
        }
    }

    pub(crate) const fn constant(minutes: i16) -> MinuteOffset {
        assert!(minutes >= -Self::MAX_MINUTES && minutes <= Self::MAX_MINUTES);
        MinuteOffset(minutes)
    }

    pub fn minutes(self) -> i16 {
        self.0
    }
}

/// Minutes added to each of the day's times.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Default, Hash)]
pub struct Offsets {
    pub fajr: MinuteOffset,
    pub sunrise: MinuteOffset,
    pub dhuhr: MinuteOffset,
    pub asr: MinuteOffset,
    pub maghrib: MinuteOffset,
    pub isha: MinuteOffset,
}

impl Default for MinuteOffset {
    fn default() -> Self {
        MinuteOffset::ZERO
    }
}

impl Offsets {
    pub const ZERO: Offsets = Offsets {
        fajr: MinuteOffset::ZERO,
        sunrise: MinuteOffset::ZERO,
        dhuhr: MinuteOffset::ZERO,
        asr: MinuteOffset::ZERO,
        maghrib: MinuteOffset::ZERO,
        isha: MinuteOffset::ZERO,
    };

    /// Fajr to Isha.
    pub(crate) fn in_order(&self) -> [MinuteOffset; 6] {
        [
            self.fajr,
            self.sunrise,
            self.dhuhr,
            self.asr,
            self.maghrib,
            self.isha,
        ]
    }
}

/// When Isha begins.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Isha {
    /// When the sun is this far below the horizon after sunset.
    Angle(DepressionAngle),
    /// This long after sunset.
    Interval(IshaInterval),
}

/// When Maghrib begins.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Maghrib {
    /// At sunset.
    Sunset,
    /// When the sun is this far below the horizon (Tehran's method), if
    /// that is five minutes before Isha; else at sunset (the time says so:
    /// [`Fallback::Sunset`](crate::Fallback::Sunset)).
    Angle(DepressionAngle),
}

/// Asr: when a stick's shadow is its noon shadow plus once (Shafi, Maliki,
/// Hanbali, Ja'fari) or twice (Hanafi) its height.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Madhab {
    Shafi,
    Hanafi,
}

impl Madhab {
    /// The shadow's length beyond its noon length, in the stick's heights.
    pub fn shadow_length(self) -> f64 {
        match self {
            Madhab::Shafi => 1.0,
            Madhab::Hanafi => 2.0,
        }
    }
}

/// Where twilight lasts long: the share of the night Fajr is no earlier
/// than, before sunrise, and Isha no later than, after sunset.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum HighLatitudeRule {
    /// Half the night.
    MiddleOfTheNight,
    /// A seventh of the night.
    SeventhOfTheNight,
    /// The twilight's angle / 60 of the night.
    TwilightAngle,
}

impl HighLatitudeRule {
    /// The seventh of the night beyond 48° north or south, else the middle.
    pub fn recommended(coordinates: Coordinates) -> HighLatitudeRule {
        match coordinates.latitude.degrees().abs() > 48.0 {
            true => HighLatitudeRule::SeventhOfTheNight,
            false => HighLatitudeRule::MiddleOfTheNight,
        }
    }

    /// The share of the night for a twilight at `angle`.
    pub(crate) fn share(self, angle: DepressionAngle) -> verified::Share {
        match self {
            HighLatitudeRule::MiddleOfTheNight => verified::Share {
                numerator: 1,
                denominator: 2,
            },
            HighLatitudeRule::SeventhOfTheNight => verified::Share {
                numerator: 1,
                denominator: 7,
            },
            HighLatitudeRule::TwilightAngle => angle.night_share(),
        }
    }
}

/// The twilight the Moonsighting Committee's Isha follows.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Shafaq {
    /// Between Ahmer and Abyad: the most reasonable times at high latitudes.
    General,
    /// The red glow (Shafi, Maliki, Hanbali): earlier.
    Ahmer,
    /// The white glow (Hanafi): later.
    Abyad,
}

/// How Fajr's earliest and Isha's latest times are bounded.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum NightBound {
    /// By a share of the night.
    HighLatitude(HighLatitudeRule),
    /// By the Moonsighting Committee's seasonal twilights (from
    /// observation), and above 55°N by a seventh of the night in place of
    /// the angles.
    MoonsightingSeasons(Shafaq),
}

/// What to do on a day without its times near the poles (no sunrise or
/// sunset, no Asr, times out of order): the Muslim World League's Fiqh
/// Council (1986) and others estimate them from a place or a day with them.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Default, Hash)]
pub enum PolarCircleResolution {
    /// The day as it is: a [`PartialDay`](crate::PartialDay) saying what's
    /// missing and why.
    #[default]
    Unresolved,
    /// The nearest latitude toward the equator, in half-degree steps along
    /// the meridian, that has the times on the day (aqrab al-bilad).
    AqrabBalad,
    /// The nearest day at the place that has them, a day after then a day
    /// before, up to half a year (aqrab al-ayyam).
    AqrabYaum,
}

/// How the times are put on a minute.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Default, Hash)]
pub enum Rounding {
    /// To the nearest minute, half a minute up.
    #[default]
    Nearest,
    /// To the next minute; a time on a minute stays.
    Up,
    /// Not at all: to the second.
    Off,
}

impl Rounding {
    pub(crate) fn core(self) -> verified::Round {
        match self {
            Rounding::Nearest => verified::Round::Nearest,
            Rounding::Up => verified::Round::Up,
            Rounding::Off => verified::Round::Exact,
        }
    }
}

/// A calculation method's settings. Build one from a [`Method`]'s preset,
/// or from angles with [`Parameters::new`], and change it with the `with_`
/// methods; every combination is valid.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub struct Parameters {
    fajr: DepressionAngle,
    isha: Isha,
    maghrib: Maghrib,
    night_bound: NightBound,
    madhab: Madhab,
    polar_circle_resolution: PolarCircleResolution,
    method_offsets: Offsets,
    offsets: Offsets,
    rounding: Rounding,
}

impl Parameters {
    /// Fajr and Isha as given; Maghrib at sunset, the middle of the night at
    /// high latitudes, Shafi's Asr, no polar circle resolution, no offsets,
    /// the nearest minute.
    pub const fn new(fajr: DepressionAngle, isha: Isha) -> Parameters {
        Parameters {
            fajr,
            isha,
            maghrib: Maghrib::Sunset,
            night_bound: NightBound::HighLatitude(HighLatitudeRule::MiddleOfTheNight),
            madhab: Madhab::Shafi,
            polar_circle_resolution: PolarCircleResolution::Unresolved,
            method_offsets: Offsets::ZERO,
            offsets: Offsets::ZERO,
            rounding: Rounding::Nearest,
        }
    }

    pub const fn with_maghrib(self, maghrib: Maghrib) -> Parameters {
        Parameters { maghrib, ..self }
    }

    pub const fn with_night_bound(self, night_bound: NightBound) -> Parameters {
        Parameters {
            night_bound,
            ..self
        }
    }

    /// The night bound by this high-latitude rule.
    pub const fn with_high_latitude_rule(self, rule: HighLatitudeRule) -> Parameters {
        self.with_night_bound(NightBound::HighLatitude(rule))
    }

    pub const fn with_madhab(self, madhab: Madhab) -> Parameters {
        Parameters { madhab, ..self }
    }

    pub const fn with_polar_circle_resolution(
        self,
        resolution: PolarCircleResolution,
    ) -> Parameters {
        Parameters {
            polar_circle_resolution: resolution,
            ..self
        }
    }

    /// The user's offsets, added to the method's.
    pub const fn with_offsets(self, offsets: Offsets) -> Parameters {
        Parameters { offsets, ..self }
    }

    /// The method's own offsets (a preset's, say: the Dubai method adds
    /// three minutes to Dhuhr).
    pub const fn with_method_offsets(self, method_offsets: Offsets) -> Parameters {
        Parameters {
            method_offsets,
            ..self
        }
    }

    pub const fn with_rounding(self, rounding: Rounding) -> Parameters {
        Parameters { rounding, ..self }
    }

    pub fn fajr(&self) -> DepressionAngle {
        self.fajr
    }

    pub fn isha(&self) -> Isha {
        self.isha
    }

    pub fn maghrib(&self) -> Maghrib {
        self.maghrib
    }

    pub fn night_bound(&self) -> NightBound {
        self.night_bound
    }

    pub fn madhab(&self) -> Madhab {
        self.madhab
    }

    pub fn polar_circle_resolution(&self) -> PolarCircleResolution {
        self.polar_circle_resolution
    }

    pub fn method_offsets(&self) -> Offsets {
        self.method_offsets
    }

    pub fn offsets(&self) -> Offsets {
        self.offsets
    }

    pub fn rounding(&self) -> Rounding {
        self.rounding
    }

    /// Each time's minutes: the method's and the user's, Fajr to Isha
    /// (each within 12 hours, so the sum within the core's 24).
    pub(crate) fn minutes(&self) -> [i32; 6] {
        let (method, user) = (self.method_offsets.in_order(), self.offsets.in_order());
        [0, 1, 2, 3, 4, 5].map(|i| i32::from(method[i].0) + i32::from(user[i].0))
    }
}

/// The calculation methods in use: each a preset of [`Parameters`].
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[derive(PartialEq, Eq, Debug, Copy, Clone, Hash)]
pub enum Method {
    /// Muslim World League: Fajr 18°, Isha 17°.
    MuslimWorldLeague,
    /// Egyptian General Authority of Survey: Fajr 19.5°, Isha 17.5°.
    Egyptian,
    /// University of Islamic Sciences, Karachi: 18° and 18°.
    Karachi,
    /// Umm al-Qura University, Makkah: Fajr 18.5°, Isha 90 minutes after
    /// Maghrib (add 30 in Ramadan).
    UmmAlQura,
    /// The UAE: 18.2° and 18.2°, sunrise −3, Dhuhr, Asr and Maghrib +3
    /// minutes.
    Dubai,
    /// Moonsighting Committee Worldwide: 18° and 18° within seasonal
    /// twilights from observation; a seventh of the night above 55°N.
    MoonsightingCommittee,
    /// ISNA: 15° and 15°.
    NorthAmerica,
    /// Kuwait: Fajr 18°, Isha 17.5°.
    Kuwait,
    /// Qatar: Fajr 18°, Isha 90 minutes after Maghrib.
    Qatar,
    /// Singapore, Malaysia, Indonesia: Fajr 20°, Isha 18°, rounded up.
    Singapore,
    /// Institute of Geophysics, University of Tehran: Fajr 17.7°, Isha 14°,
    /// Maghrib at 4.5°.
    Tehran,
    /// An approximation of Turkey's Diyanet: 18° and 17°, with offsets.
    Turkey,
}

impl Method {
    pub const ALL: [Method; 12] = [
        Method::MuslimWorldLeague,
        Method::Egyptian,
        Method::Karachi,
        Method::UmmAlQura,
        Method::Dubai,
        Method::MoonsightingCommittee,
        Method::NorthAmerica,
        Method::Kuwait,
        Method::Qatar,
        Method::Singapore,
        Method::Tehran,
        Method::Turkey,
    ];

    pub const fn parameters(self) -> Parameters {
        match self {
            Method::MuslimWorldLeague => presets::MUSLIM_WORLD_LEAGUE,
            Method::Egyptian => presets::EGYPTIAN,
            Method::Karachi => presets::KARACHI,
            Method::UmmAlQura => presets::UMM_AL_QURA,
            Method::Dubai => presets::DUBAI,
            Method::MoonsightingCommittee => presets::MOONSIGHTING_COMMITTEE,
            Method::NorthAmerica => presets::NORTH_AMERICA,
            Method::Kuwait => presets::KUWAIT,
            Method::Qatar => presets::QATAR,
            Method::Singapore => presets::SINGAPORE,
            Method::Tehran => presets::TEHRAN,
            Method::Turkey => presets::TURKEY,
        }
    }
}

/// The methods' parameters, as constants: valid by construction (every
/// value is checked when the crate compiles).
pub mod presets {
    use super::*;

    const fn angle(hundredths: u16) -> DepressionAngle {
        DepressionAngle::hundredths(hundredths)
    }

    const fn minutes(minutes: i16) -> MinuteOffset {
        MinuteOffset::constant(minutes)
    }

    const DHUHR_PLUS_ONE: Offsets = Offsets {
        dhuhr: minutes(1),
        ..Offsets::ZERO
    };

    pub const MUSLIM_WORLD_LEAGUE: Parameters =
        Parameters::new(angle(1800), Isha::Angle(angle(1700))).with_method_offsets(DHUHR_PLUS_ONE);
    pub const EGYPTIAN: Parameters =
        Parameters::new(angle(1950), Isha::Angle(angle(1750))).with_method_offsets(DHUHR_PLUS_ONE);
    pub const KARACHI: Parameters =
        Parameters::new(angle(1800), Isha::Angle(angle(1800))).with_method_offsets(DHUHR_PLUS_ONE);
    pub const UMM_AL_QURA: Parameters =
        Parameters::new(angle(1850), Isha::Interval(IshaInterval::constant(90)));
    pub const DUBAI: Parameters = Parameters::new(angle(1820), Isha::Angle(angle(1820)))
        .with_method_offsets(Offsets {
            sunrise: minutes(-3),
            dhuhr: minutes(3),
            asr: minutes(3),
            maghrib: minutes(3),
            ..Offsets::ZERO
        });
    pub const MOONSIGHTING_COMMITTEE: Parameters =
        Parameters::new(angle(1800), Isha::Angle(angle(1800)))
            .with_night_bound(NightBound::MoonsightingSeasons(Shafaq::General))
            .with_method_offsets(Offsets {
                dhuhr: minutes(5),
                maghrib: minutes(3),
                ..Offsets::ZERO
            });
    pub const NORTH_AMERICA: Parameters =
        Parameters::new(angle(1500), Isha::Angle(angle(1500))).with_method_offsets(DHUHR_PLUS_ONE);
    pub const KUWAIT: Parameters = Parameters::new(angle(1800), Isha::Angle(angle(1750)));
    pub const QATAR: Parameters =
        Parameters::new(angle(1800), Isha::Interval(IshaInterval::constant(90)));
    pub const SINGAPORE: Parameters = Parameters::new(angle(2000), Isha::Angle(angle(1800)))
        .with_method_offsets(DHUHR_PLUS_ONE)
        .with_rounding(Rounding::Up);
    pub const TEHRAN: Parameters = Parameters::new(angle(1770), Isha::Angle(angle(1400)))
        .with_maghrib(Maghrib::Angle(angle(450)));
    pub const TURKEY: Parameters = Parameters::new(angle(1800), Isha::Angle(angle(1700)))
        .with_method_offsets(Offsets {
            sunrise: minutes(-7),
            dhuhr: minutes(5),
            asr: minutes(4),
            maghrib: minutes(7),
            ..Offsets::ZERO
        });
}

macro_rules! conversions {
    ($type:ident, $raw:ty, $error:ident, $what:literal) => {
        impl TryFrom<$raw> for $type {
            type Error = $error;

            fn try_from(raw: $raw) -> Result<Self, $error> {
                $type::new(raw)
            }
        }

        impl fmt::Display for $error {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{} is not {}", self.0, $what)
            }
        }

        impl core::error::Error for $error {}
    };
}

conversions!(
    DepressionAngle,
    f64,
    AngleError,
    "a twilight's degrees below the horizon in (0, 30]"
);
conversions!(
    IshaInterval,
    u8,
    IntervalError,
    "Isha's minutes after sunset in [5, 240]"
);
conversions!(
    MinuteOffset,
    i16,
    OffsetError,
    "an offset in minutes in [-720, 720]"
);

impl From<DepressionAngle> for f64 {
    fn from(angle: DepressionAngle) -> f64 {
        angle.degrees()
    }
}

impl From<IshaInterval> for u8 {
    fn from(interval: IshaInterval) -> u8 {
        interval.0
    }
}

impl From<MinuteOffset> for i16 {
    fn from(offset: MinuteOffset) -> i16 {
        offset.0
    }
}

#[cfg(kani)]
impl kani::Arbitrary for DepressionAngle {
    fn any() -> Self {
        let hundredths: u16 = kani::any();
        kani::assume((1..=3000).contains(&hundredths));
        DepressionAngle(hundredths)
    }
}

#[cfg(kani)]
impl kani::Arbitrary for IshaInterval {
    fn any() -> Self {
        let minutes: u8 = kani::any();
        kani::assume((5..=240).contains(&minutes));
        IshaInterval(minutes)
    }
}

#[cfg(kani)]
impl kani::Arbitrary for MinuteOffset {
    fn any() -> Self {
        let minutes: i16 = kani::any();
        kani::assume((-720..=720).contains(&minutes));
        MinuteOffset(minutes)
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any f64 is an angle exactly when it rounds to (0, 30] in
    /// hundredths, then a share of the night of at most a half.
    #[kani::proof]
    fn angle_new_is_total() {
        let degrees: f64 = kani::any();
        match DepressionAngle::new(degrees) {
            Ok(angle) => {
                assert!((1..=3000).contains(&angle.0));
                let share = angle.night_share();
                assert!(2 * share.numerator <= share.denominator);
            }
            Err(AngleError(rejected)) => {
                assert!(rejected.to_bits() == degrees.to_bits());
            }
        }
    }

    /// Any parameters' offsets are each within the core's 24 hours.
    #[kani::proof]
    fn offsets_are_within_the_core_bound() {
        let parameters: Parameters = kani::any();
        for minutes in parameters.minutes() {
            assert!((-1440..=1440).contains(&minutes));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn angles_are_hundredths_of_a_degree_in_0_to_30() {
        // salah 0.7 took any f64: NaN fell back to the safe time silently,
        // and a huge angle under the twilight angle rule overflowed.
        for bad in [
            f64::NAN,
            f64::INFINITY,
            -f64::INFINITY,
            -0.5,
            0.0,
            30.01,
            1e300,
        ] {
            assert!(DepressionAngle::new(bad).is_err(), "{bad}");
        }
        assert_eq!(DepressionAngle::new(18.2).unwrap().degrees(), 18.2);
        assert_eq!(DepressionAngle::new(17.704).unwrap().degrees(), 17.7);
        assert_eq!(
            DepressionAngle::new(30.0).unwrap(),
            DepressionAngle::hundredths(3000)
        );
    }

    #[test]
    fn intervals_and_offsets_are_in_range() {
        assert!(IshaInterval::new(4).is_err());
        assert!(IshaInterval::new(241).is_err());
        assert_eq!(IshaInterval::new(90).unwrap().minutes(), 90);
        assert!(MinuteOffset::new(721).is_err());
        assert!(MinuteOffset::new(-721).is_err());
        assert_eq!(MinuteOffset::new(-30).unwrap().minutes(), -30);
    }

    #[test]
    fn presets() {
        let mwl = Method::MuslimWorldLeague.parameters();
        assert_eq!(mwl.fajr().degrees(), 18.0);
        assert_eq!(mwl.isha(), Isha::Angle(DepressionAngle::hundredths(1700)));
        assert_eq!(mwl.method_offsets().dhuhr.minutes(), 1);
        let tehran = Method::Tehran.parameters();
        assert_eq!(
            tehran.maghrib(),
            Maghrib::Angle(DepressionAngle::hundredths(450))
        );
        assert_eq!(Method::Singapore.parameters().rounding(), Rounding::Up);
        assert_eq!(
            Method::UmmAlQura.parameters().isha(),
            Isha::Interval(IshaInterval::constant(90))
        );
        assert_eq!(
            Method::MoonsightingCommittee.parameters().night_bound(),
            NightBound::MoonsightingSeasons(Shafaq::General)
        );
    }

    #[test]
    fn night_shares() {
        let angle = DepressionAngle::hundredths(1500);
        let share = HighLatitudeRule::TwilightAngle.share(angle);
        assert_eq!((share.numerator, share.denominator), (1500, 6000));
        let seventh = HighLatitudeRule::SeventhOfTheNight.share(angle);
        assert_eq!((seventh.numerator, seventh.denominator), (1, 7));
    }

    #[test]
    fn recommended_rule_in_both_hemispheres() {
        // salah 0.7 only looked north of 48°.
        for latitude in [48.98, -51.62] {
            let place = Coordinates::from_degrees(latitude, 0.0).unwrap();
            assert_eq!(
                HighLatitudeRule::recommended(place),
                HighLatitudeRule::SeventhOfTheNight
            );
        }
        let place = Coordinates::from_degrees(45.98, 0.0).unwrap();
        assert_eq!(
            HighLatitudeRule::recommended(place),
            HighLatitudeRule::MiddleOfTheNight
        );
    }

    #[cfg(feature = "serde")]
    #[test]
    fn deserializing_checks_values() {
        assert_eq!(
            serde_json::from_str::<DepressionAngle>("18.5").unwrap(),
            DepressionAngle::hundredths(1850)
        );
        assert!(serde_json::from_str::<DepressionAngle>("95").is_err());
        assert!(serde_json::from_str::<IshaInterval>("2").is_err());
        assert!(serde_json::from_str::<MinuteOffset>("1000").is_err());
    }
}
