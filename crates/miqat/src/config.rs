//! `config.toml` decoded into domain types at the boundary.

use std::net::SocketAddr;
use std::path::Path;

use anyhow::Context;
use chrono_tz::Tz;
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub location: Location,
    pub prayer: PrayerConfig,
    #[serde(default)]
    pub weather: WeatherConfig,
    #[serde(default)]
    pub radar: RadarConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub server: ServerConfig,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub latitude: Latitude,
    pub longitude: Longitude,
    #[serde(with = "tz_name")]
    pub timezone: Tz,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "f64")]
pub struct Latitude(f64);

impl Latitude {
    pub fn degrees(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Latitude {
    type Error = String;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        // Web Mercator (the radar map) is undefined past ±85.05°.
        (-85.0..=85.0)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| format!("latitude {value} is outside -85..=85"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(try_from = "f64")]
pub struct Longitude(f64);

impl Longitude {
    pub fn degrees(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Longitude {
    type Error = String;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        (-180.0..=180.0)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| format!("longitude {value} is outside -180..=180"))
    }
}

mod tz_name {
    use chrono_tz::Tz;
    use serde::{Deserialize, Deserializer, de::Error};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Tz, D::Error> {
        let name = String::deserialize(d)?;
        name.parse()
            .map_err(|_| D::Error::custom(format!("unknown timezone {name:?}")))
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrayerConfig {
    pub method: CalculationMethod,
    #[serde(default)]
    pub madhab: Madhab,
    /// Shifts the tabular Hijri date to match local moon sighting.
    #[serde(default)]
    pub hijri_offset_days: i8,
    #[serde(default)]
    pub adjust_minutes: PrayerAdjustments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalculationMethod {
    MuslimWorldLeague,
    Egyptian,
    Karachi,
    UmmAlQura,
    Dubai,
    MoonsightingCommittee,
    NorthAmerica,
    Kuwait,
    Qatar,
    Singapore,
    Tehran,
    Turkey,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Madhab {
    #[default]
    Shafi,
    Hanafi,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PrayerAdjustments {
    pub fajr: i8,
    pub sunrise: i8,
    pub dhuhr: i8,
    pub asr: i8,
    pub maghrib: i8,
    pub isha: i8,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct WeatherConfig {
    pub temperature_unit: TemperatureUnit,
    pub wind_unit: WindUnit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TemperatureUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindUnit {
    #[default]
    Kmh,
    Ms,
    Mph,
    Kn,
}

impl WindUnit {
    /// Open-Meteo's `wind_speed_unit` value; it matches our serde name.
    pub fn api_name(self) -> &'static str {
        match self {
            WindUnit::Kmh => "kmh",
            WindUnit::Ms => "ms",
            WindUnit::Mph => "mph",
            WindUnit::Kn => "kn",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WindUnit::Kmh => "km/h",
            WindUnit::Ms => "m/s",
            WindUnit::Mph => "mph",
            WindUnit::Kn => "kn",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RadarConfig {
    pub zoom: Zoom,
}

impl Default for RadarConfig {
    fn default() -> Self {
        Self { zoom: Zoom(8) }
    }
}

/// Map zoom. Below 3 the map is a continent; past 9 the upscaled radar is mush.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(try_from = "u8")]
pub struct Zoom(u8);

/// RainViewer's free tiles stop at zoom 7 (since 2026); deeper maps upscale its radar.
const MAX_RADAR_ZOOM: Zoom = Zoom(7);

impl Zoom {
    pub fn level(self) -> u8 {
        self.0
    }

    /// The zoom radar tiles are fetched at.
    pub fn radar(self) -> Zoom {
        self.min(MAX_RADAR_ZOOM)
    }
}

impl TryFrom<u8> for Zoom {
    type Error = String;
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        (3..=9)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| format!("radar zoom {value} is outside 3..=9"))
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DisplayConfig {
    pub clock: ClockFormat,
    pub contrast: Contrast,
}

/// How hard the mid-tones are darkened before the frame reaches the paperwhite.
///
/// The panel has 16 gray levels and lifts the middle of the range well above
/// their 8-bit value, so a `#666` label reads as pale haze rather than text.
/// This is the gamma applied to the frame to cancel that lift: black and white
/// stay put and everything between them comes down.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Deserialize)]
#[serde(try_from = "f64")]
pub struct Contrast(f64);

impl Default for Contrast {
    fn default() -> Self {
        Self(1.5)
    }
}

impl Contrast {
    /// The gamma applied to an 8-bit gray value. 1.0 leaves it alone; higher
    /// values pull the midtones down toward black while leaving black and
    /// white where they are.
    pub fn darken(self, value: u8) -> u8 {
        let scaled = f64::from(value) / 255.0;
        (255.0 * scaled.powf(self.0)).round() as u8
    }
}

impl TryFrom<f64> for Contrast {
    type Error = String;
    fn try_from(value: f64) -> Result<Self, Self::Error> {
        (1.0..=3.0)
            .contains(&value)
            .then_some(Self(value))
            .ok_or_else(|| format!("display contrast {value} is outside 1.0..=3.0"))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum ClockFormat {
    #[serde(rename = "12h")]
    TwelveHour,
    #[default]
    #[serde(rename = "24h")]
    TwentyFourHour,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServerConfig {
    pub listen: SocketAddr,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([0, 0, 0, 0], 8080)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let config: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        assert_eq!(config.prayer.method, CalculationMethod::MuslimWorldLeague);
        assert_eq!(config.radar.zoom.level(), 8);
    }

    #[test]
    fn rejects_out_of_range_values() {
        let base = "[location]\nlatitude = 91.0\nlongitude = 0.0\ntimezone = \"UTC\"\n[prayer]\nmethod = \"egyptian\"\n";
        assert!(toml::from_str::<Config>(base).is_err());
        let zoom = base.replace("91.0", "30.0") + "[radar]\nzoom = 10\n";
        assert!(toml::from_str::<Config>(&zoom).is_err());
        let tz = base.replace("91.0", "30.0").replace("UTC", "Mars/Olympus");
        assert!(toml::from_str::<Config>(&tz).is_err());
        let contrast = base.replace("91.0", "30.0") + "[display]\ncontrast = 0.5\n";
        assert!(toml::from_str::<Config>(&contrast).is_err());
    }

    #[test]
    fn contrast_keeps_the_ends_and_darkens_the_middle() {
        let contrast = Contrast::default();
        assert_eq!(contrast.darken(0), 0);
        assert_eq!(contrast.darken(255), 255);
        // The panel lifts midtones, so the curve has to pull them down.
        assert!(contrast.darken(102) < 102);
        // Monotonic, so the 16 levels stay in order.
        let levels: Vec<u8> = (0..=255).map(|v| contrast.darken(v)).collect();
        assert!(levels.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn contrast_of_one_is_a_no_op() {
        let contrast = Contrast::try_from(1.0).unwrap();
        assert_eq!(
            (0..=255).map(|v| contrast.darken(v)).collect::<Vec<u8>>(),
            (0..=255).collect::<Vec<u8>>()
        );
    }
}
