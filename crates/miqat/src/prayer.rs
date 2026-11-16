//! Prayer times, computed locally with the `salah` crate (a port of Adhan).

use chrono::{DateTime, NaiveDate, Utc, Weekday};
use salah::{Configuration, Coordinates, Parameters, Prayer, PrayerSchedule};

use crate::config::{CalculationMethod, Location, Madhab, PrayerConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Salah {
    Fajr,
    Sunrise,
    Dhuhr,
    Asr,
    Maghrib,
    Isha,
}

impl Salah {
    pub const DAY: [Salah; 6] = [
        Salah::Fajr,
        Salah::Sunrise,
        Salah::Dhuhr,
        Salah::Asr,
        Salah::Maghrib,
        Salah::Isha,
    ];

    /// (English, Arabic) names; Dhuhr is Jumu'ah on Fridays.
    pub fn names(self, weekday: Weekday) -> (&'static str, &'static str) {
        match (self, weekday) {
            (Salah::Fajr, _) => ("Fajr", "الفجر"),
            (Salah::Sunrise, _) => ("Sunrise", "الشروق"),
            (Salah::Dhuhr, Weekday::Fri) => ("Jumu'ah", "الجمعة"),
            (Salah::Dhuhr, _) => ("Dhuhr", "الظهر"),
            (Salah::Asr, _) => ("Asr", "العصر"),
            (Salah::Maghrib, _) => ("Maghrib", "المغرب"),
            (Salah::Isha, _) => ("Isha", "العشاء"),
        }
    }

    fn library(self) -> Prayer {
        match self {
            Salah::Fajr => Prayer::Fajr,
            Salah::Sunrise => Prayer::Sunrise,
            Salah::Dhuhr => Prayer::Dhuhr,
            Salah::Asr => Prayer::Asr,
            Salah::Maghrib => Prayer::Maghrib,
            Salah::Isha => Prayer::Isha,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DayTimes {
    pub date: NaiveDate,
    /// Indexed by `Salah as usize`; the array has one slot per variant.
    times: [DateTime<Utc>; 6],
    /// Fajr of the following day, the next prayer once Isha has passed.
    pub fajr_tomorrow: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Upcoming {
    pub prayer: Salah,
    pub at: DateTime<Utc>,
}

impl DayTimes {
    pub fn compute(
        location: &Location,
        config: &PrayerConfig,
        date: NaiveDate,
    ) -> anyhow::Result<Self> {
        let schedule = PrayerSchedule::new()
            .on(date)
            .for_location(Coordinates::new(
                location.latitude.degrees(),
                location.longitude.degrees(),
            ))
            .with_configuration(parameters(config))
            .calculate()
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            date,
            times: Salah::DAY.map(|s| schedule.time(s.library())),
            fajr_tomorrow: schedule.time(Prayer::FajrTomorrow),
        })
    }

    pub fn at(&self, salah: Salah) -> DateTime<Utc> {
        self.times[salah as usize]
    }

    pub fn times(&self) -> [(Salah, DateTime<Utc>); 6] {
        Salah::DAY.map(|s| (s, self.at(s)))
    }

    pub fn upcoming(&self, now: DateTime<Utc>) -> Upcoming {
        self.times()
            .iter()
            .find(|(_, at)| *at > now)
            .map(|&(prayer, at)| Upcoming { prayer, at })
            .unwrap_or(Upcoming {
                prayer: Salah::Fajr,
                at: self.fajr_tomorrow,
            })
    }

    /// The day's times as shown at `now`: once Isha has passed, Fajr becomes tomorrow's.
    pub fn board(&self, now: DateTime<Utc>) -> [(Salah, DateTime<Utc>); 6] {
        let after_isha = self.upcoming(now).at == self.fajr_tomorrow;
        self.times().map(|(salah, at)| match salah {
            Salah::Fajr if after_isha => (salah, self.fajr_tomorrow),
            _ => (salah, at),
        })
    }

    /// The Islamic day begins at Maghrib, so the Hijri date advances then.
    pub fn islamic_date(&self, now: DateTime<Utc>) -> NaiveDate {
        if now >= self.at(Salah::Maghrib) {
            self.date.succ_opt().unwrap_or(self.date)
        } else {
            self.date
        }
    }
}

fn parameters(config: &PrayerConfig) -> Parameters {
    let a = config.adjust_minutes;
    let method = match config.method {
        CalculationMethod::MuslimWorldLeague => salah::Method::MuslimWorldLeague,
        CalculationMethod::Egyptian => salah::Method::Egyptian,
        CalculationMethod::Karachi => salah::Method::Karachi,
        CalculationMethod::UmmAlQura => salah::Method::UmmAlQura,
        CalculationMethod::Dubai => salah::Method::Dubai,
        CalculationMethod::MoonsightingCommittee => salah::Method::MoonsightingCommittee,
        CalculationMethod::NorthAmerica => salah::Method::NorthAmerica,
        CalculationMethod::Kuwait => salah::Method::Kuwait,
        CalculationMethod::Qatar => salah::Method::Qatar,
        CalculationMethod::Singapore => salah::Method::Singapore,
        CalculationMethod::Tehran => salah::Method::Tehran,
        CalculationMethod::Turkey => salah::Method::Turkey,
    };
    let madhab = match config.madhab {
        Madhab::Shafi => salah::Madhab::Shafi,
        Madhab::Hanafi => salah::Madhab::Hanafi,
    };
    let mut params = Configuration::with(method, madhab);
    params.adjustments = salah::TimeAdjustment::new(
        a.fajr.into(),
        a.sunrise.into(),
        a.dhuhr.into(),
        a.asr.into(),
        a.maghrib.into(),
        a.isha.into(),
    );
    params
}

/// The local calendar day `now` falls on.
pub fn local_date(location: &Location, now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&location.timezone).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, PrayerAdjustments};
    use chrono::TimeZone;

    fn amsterdam() -> (Location, PrayerConfig) {
        let config: Config = toml::from_str(
            r#"
            [location]
            latitude = 52.37
            longitude = 4.90
            timezone = "Europe/Amsterdam"
            [prayer]
            method = "muslim_world_league"
            "#,
        )
        .unwrap();
        (config.location, config.prayer)
    }

    #[test]
    fn times_are_ordered_and_plausible() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        assert!(day.times().windows(2).all(|w| w[0].1 < w[1].1));
        assert!(day.at(Salah::Isha) < day.fajr_tomorrow);
        let dhuhr = day.at(Salah::Dhuhr).with_timezone(&location.timezone);
        assert_eq!(
            dhuhr.format("%H").to_string(),
            "12",
            "Dhuhr in Amsterdam in November"
        );
    }

    #[test]
    fn upcoming_rolls_over_to_tomorrow_after_isha() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        let before_asr = day.at(Salah::Asr) - chrono::Duration::minutes(1);
        assert_eq!(day.upcoming(before_asr).prayer, Salah::Asr);
        let at_asr = day.at(Salah::Asr);
        assert_eq!(day.upcoming(at_asr).prayer, Salah::Maghrib);
        let late = Utc.with_ymd_and_hms(2026, 11, 11, 23, 0, 0).unwrap();
        assert_eq!(
            day.upcoming(late),
            Upcoming {
                prayer: Salah::Fajr,
                at: day.fajr_tomorrow
            }
        );
    }

    #[test]
    fn after_isha_the_board_shows_tomorrows_fajr_and_the_next_hijri_day() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let day = DayTimes::compute(&location, &config, date).unwrap();
        let afternoon = day.at(Salah::Asr);
        assert_eq!(day.board(afternoon), day.times());
        assert_eq!(day.islamic_date(afternoon), date);
        let night = day.at(Salah::Isha) + chrono::Duration::minutes(5);
        assert_eq!(day.board(night)[0], (Salah::Fajr, day.fajr_tomorrow));
        assert_eq!(day.islamic_date(night), date.succ_opt().unwrap());
    }

    #[test]
    fn adjustments_shift_times() {
        let (location, config) = amsterdam();
        let date = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let base = DayTimes::compute(&location, &config, date).unwrap();
        let shifted = PrayerConfig {
            adjust_minutes: PrayerAdjustments {
                isha: 10,
                ..Default::default()
            },
            ..config
        };
        let later = DayTimes::compute(&location, &shifted, date).unwrap();
        assert_eq!(
            later.at(Salah::Isha) - base.at(Salah::Isha),
            chrono::Duration::minutes(10)
        );
    }

    #[test]
    fn friday_names_jumuah() {
        assert_eq!(Salah::Dhuhr.names(Weekday::Fri).0, "Jumu'ah");
        assert_eq!(Salah::Dhuhr.names(Weekday::Thu).0, "Dhuhr");
    }
}
