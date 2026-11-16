//! Open-Meteo forecast, decoded into domain types at the boundary.

use anyhow::{Context, bail};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use chrono_tz::Tz;

use crate::config::{Location, TemperatureUnit, WeatherConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct Weather {
    pub current: Current,
    pub days: Vec<DayRange>,
    pub hours: Vec<Hour>,
    /// Precipitation per 15 minutes, starting at the current quarter hour.
    pub quarters: Vec<Quarter>,
}

/// Rain only earns space on the dashboard when a quarter hour is at least this wet.
pub const WET_MM: f64 = 0.1;
/// How far ahead the dashboard looks for rain.
pub const RAIN_QUARTERS: usize = 8;

impl Weather {
    /// Whether any quarter hour in the next two hours is wet enough to count as rain.
    /// The outlook line and the radar card share this so they can never disagree.
    pub fn rain_expected(&self, now: DateTime<Utc>) -> bool {
        let start = self
            .quarters
            .partition_point(|quarter| quarter.at + Duration::minutes(15) <= now);
        self.quarters[start..(start + RAIN_QUARTERS).min(self.quarters.len())]
            .iter()
            .any(|quarter| quarter.precipitation_mm >= WET_MM)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Current {
    pub temperature: f64,
    pub feels_like: f64,
    pub humidity_percent: f64,
    pub sky: Sky,
    pub wind: Wind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wind {
    pub speed: f64,
    pub gust: f64,
    /// Meteorological convention: where the wind blows from, clockwise from north.
    pub from_degrees: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sky {
    pub condition: Condition,
    pub light: Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    Day,
    Night,
}

impl Light {
    /// Open-Meteo's `is_day` flag.
    fn from_is_day(flag: u8) -> anyhow::Result<Self> {
        match flag {
            1 => Ok(Light::Day),
            0 => Ok(Light::Night),
            other => bail!("is_day must be 0 or 1, got {other}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DayRange {
    pub date: NaiveDate,
    pub high: f64,
    pub low: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hour {
    pub at: DateTime<Utc>,
    pub temperature: f64,
    pub precipitation_chance: Option<f64>,
    pub sky: Sky,
    pub wind_speed: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quarter {
    pub at: DateTime<Utc>,
    pub precipitation_mm: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Condition {
    Clear,
    MainlyClear,
    PartlyCloudy,
    Overcast,
    Fog,
    Drizzle,
    LightRain,
    Rain,
    HeavyRain,
    FreezingRain,
    Snow,
    HeavySnow,
    Thunderstorm,
}

impl Condition {
    /// WMO weather interpretation codes, as documented by Open-Meteo.
    fn from_wmo(code: u8) -> anyhow::Result<Self> {
        Ok(match code {
            0 => Condition::Clear,
            1 => Condition::MainlyClear,
            2 => Condition::PartlyCloudy,
            3 => Condition::Overcast,
            45 | 48 => Condition::Fog,
            51 | 53 | 55 | 56 | 57 => Condition::Drizzle,
            61 | 80 => Condition::LightRain,
            63 | 81 => Condition::Rain,
            65 | 82 => Condition::HeavyRain,
            66 | 67 => Condition::FreezingRain,
            71 | 73 | 77 | 85 => Condition::Snow,
            75 | 86 => Condition::HeavySnow,
            95 | 96 | 99 => Condition::Thunderstorm,
            other => bail!("unknown WMO weather code {other}"),
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Condition::Clear => "Clear",
            Condition::MainlyClear => "Mostly clear",
            Condition::PartlyCloudy => "Partly cloudy",
            Condition::Overcast => "Cloudy",
            Condition::Fog => "Fog",
            Condition::Drizzle => "Drizzle",
            Condition::LightRain => "Light rain",
            Condition::Rain => "Rain",
            Condition::HeavyRain => "Heavy rain",
            Condition::FreezingRain => "Freezing rain",
            Condition::Snow => "Snow",
            Condition::HeavySnow => "Heavy snow",
            Condition::Thunderstorm => "Thunderstorm",
        }
    }
}

pub fn request_url(location: &Location, config: &WeatherConfig) -> String {
    let temperature_unit = match config.temperature_unit {
        TemperatureUnit::Celsius => "celsius",
        TemperatureUnit::Fahrenheit => "fahrenheit",
    };
    // Two decimals (~1 km) is plenty for a forecast and keeps the exact address out of
    // a third party's logs.
    format!(
        "https://api.open-meteo.com/v1/forecast?latitude={:.2}&longitude={:.2}&timezone={}\
         &timeformat=unixtime&forecast_days=2&forecast_minutely_15=12\
         &temperature_unit={temperature_unit}&wind_speed_unit={}\
         &current=temperature_2m,apparent_temperature,relative_humidity_2m,weather_code,is_day,wind_speed_10m,wind_gusts_10m,wind_direction_10m\
         &hourly=temperature_2m,precipitation_probability,weather_code,is_day,wind_speed_10m\
         &daily=temperature_2m_max,temperature_2m_min\
         &minutely_15=precipitation",
        location.latitude.degrees(),
        location.longitude.degrees(),
        location.timezone.name().replace('/', "%2F"),
        config.wind_unit.api_name(),
    )
}

pub fn decode(body: &str, timezone: Tz) -> anyhow::Result<Weather> {
    let wire: wire::Forecast = serde_json::from_str(body).context("decoding Open-Meteo JSON")?;
    let c = wire.current;
    let current = Current {
        temperature: c.temperature_2m,
        feels_like: c.apparent_temperature,
        humidity_percent: c.relative_humidity_2m,
        sky: Sky {
            condition: Condition::from_wmo(c.weather_code)?,
            light: Light::from_is_day(c.is_day)?,
        },
        wind: Wind {
            speed: c.wind_speed_10m,
            gust: c.wind_gusts_10m,
            from_degrees: c.wind_direction_10m,
        },
    };
    let d = wire.daily;
    let days = zip3(&d.time, &d.temperature_2m_max, &d.temperature_2m_min)
        .map(|(t, high, low)| {
            Ok(DayRange {
                date: timestamp(t)?.with_timezone(&timezone).date_naive(),
                high,
                low,
            })
        })
        .collect::<anyhow::Result<_>>()?;
    let h = wire.hourly;
    let hours = h
        .time
        .iter()
        .zip(&h.temperature_2m)
        .zip(&h.precipitation_probability)
        .zip(h.weather_code.iter().zip(&h.is_day))
        .zip(&h.wind_speed_10m)
        .map(
            |((((&t, &temperature), &chance), (&code, &is_day)), &wind_speed)| {
                Ok(Hour {
                    at: timestamp(t)?,
                    temperature,
                    precipitation_chance: chance,
                    sky: Sky {
                        condition: Condition::from_wmo(code)?,
                        light: Light::from_is_day(is_day)?,
                    },
                    wind_speed,
                })
            },
        )
        .collect::<anyhow::Result<_>>()?;
    let m = wire.minutely_15;
    let quarters = m
        .time
        .iter()
        .zip(&m.precipitation)
        .map(|(&t, &mm)| {
            Ok(Quarter {
                at: timestamp(t)?,
                precipitation_mm: mm.unwrap_or(0.0),
            })
        })
        .collect::<anyhow::Result<_>>()?;
    Ok(Weather {
        current,
        days,
        hours,
        quarters,
    })
}

fn timestamp(seconds: i64) -> anyhow::Result<DateTime<Utc>> {
    DateTime::from_timestamp(seconds, 0)
        .with_context(|| format!("timestamp {seconds} out of range"))
}

fn zip3<'a>(
    a: &'a [i64],
    b: &'a [f64],
    c: &'a [f64],
) -> impl Iterator<Item = (i64, f64, f64)> + 'a {
    a.iter().zip(b).zip(c).map(|((&a, &b), &c)| (a, b, c))
}

/// Open-Meteo's response shape; only used to decode.
mod wire {
    use serde::Deserialize;

    #[derive(Deserialize)]
    pub struct Forecast {
        pub current: Current,
        pub hourly: Hourly,
        pub daily: Daily,
        pub minutely_15: Minutely15,
    }

    #[derive(Deserialize)]
    pub struct Current {
        pub temperature_2m: f64,
        pub apparent_temperature: f64,
        pub relative_humidity_2m: f64,
        pub weather_code: u8,
        pub is_day: u8,
        pub wind_speed_10m: f64,
        pub wind_gusts_10m: f64,
        pub wind_direction_10m: f64,
    }

    #[derive(Deserialize)]
    pub struct Hourly {
        pub time: Vec<i64>,
        pub temperature_2m: Vec<f64>,
        pub precipitation_probability: Vec<Option<f64>>,
        pub weather_code: Vec<u8>,
        pub is_day: Vec<u8>,
        pub wind_speed_10m: Vec<f64>,
    }

    #[derive(Deserialize)]
    pub struct Daily {
        pub time: Vec<i64>,
        pub temperature_2m_max: Vec<f64>,
        pub temperature_2m_min: Vec<f64>,
    }

    #[derive(Deserialize)]
    pub struct Minutely15 {
        pub time: Vec<i64>,
        pub precipitation: Vec<Option<f64>>,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_fixture() {
        let body = include_str!("../tests/fixtures/open-meteo.json");
        let weather = decode(body, chrono_tz::Europe::Amsterdam).unwrap();
        assert_eq!(weather.days.len(), 2);
        assert_eq!(weather.hours.len(), 48);
        assert_eq!(weather.quarters.len(), 12);
        assert!(weather.current.wind.gust >= weather.current.wind.speed);
    }

    #[test]
    fn rain_expected_looks_two_hours_ahead() {
        let body = include_str!("../tests/fixtures/open-meteo.json");
        let mut weather = decode(body, chrono_tz::Europe::Amsterdam).unwrap();
        let start = weather.quarters[0].at;
        let now = start + Duration::minutes(5);
        let quarter = |i: i64, mm: f64| Quarter {
            at: start + Duration::minutes(15 * i),
            precipitation_mm: mm,
        };
        weather.quarters = (0..12).map(|i| quarter(i, 0.0)).collect();
        assert!(!weather.rain_expected(now));
        // A wet quarter inside the window counts...
        weather.quarters[7] = quarter(7, WET_MM);
        assert!(weather.rain_expected(now));
        // ...but one past the two hours does not.
        weather.quarters[7] = quarter(7, 0.0);
        weather.quarters[8] = quarter(8, WET_MM);
        assert!(!weather.rain_expected(now));
        // The quarter in progress counts.
        weather.quarters[0] = quarter(0, 1.0);
        assert!(weather.rain_expected(now));
    }

    #[test]
    fn unknown_weather_code_is_an_error() {
        assert!(Condition::from_wmo(42).is_err());
    }
}
